use super::super::sync::low_level::{Lock, Once};
use std::{
    cell::{Cell, UnsafeCell},
    future::Future,
    io,
    marker::PhantomPinned,
    mem,
    pin::Pin,
    ptr::NonNull,
    sync::{
        atomic::{AtomicBool, AtomicUsize, Ordering},
        Arc,
    },
    task::{Context, Poll, Waker},
    time::Duration,
};

#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum IoKind {
    Write = 0,
    Read = 1,
}

#[derive(Copy, Clone)]
struct IoWakerState {
    slot: bool,
    stored: bool,
    notified: bool,
    epoch: usize,
}

impl Into<usize> for IoWakerState {
    fn into(self) -> usize {
        ((self.slot as usize) << 0)
            | ((self.stored as usize) << 1)
            | ((self.notified as usize) << 2)
            | ((self.epoch & (usize::MAX >> 3)) << 3)
    }
}

impl From<usize> for IoWakerState {
    fn from(value: usize) -> Self {
        Self {
            slot: value & (1 << 0) != 0,
            stored: value & (1 << 1) != 0,
            notified: value & (1 << 2) != 0,
            epoch: value >> 3,
        }
    }
}

struct IoWaker {
    state: AtomicUsize,
    wakers: [UnsafeCell<Option<Waker>>; 2],
}

impl IoWaker {
    const fn new() -> Self {
        Self {
            state: AtomicUsize::new(0),
            wakers: [UnsafeCell::new(None), UnsafeCell::new(None)],
        }
    }

    unsafe fn poll(
        &self,
        waker_ref: &Waker,
        begin_wait: impl FnOnce(),
        cancel_wait: impl FnOnce(),
    ) -> Poll<usize> {
        let mut state: IoWakerState = self.state.load(Ordering::Acquire).into();
        if state.notified {
            return Poll::Ready(state.epoch);
        }

        if !state.stored {
            let slot_ptr = self.wakers[state.slot as usize].get();
            assert!((&*slot_ptr).as_ref().is_none());
            *slot_ptr = Some(waker_ref.clone());

            match self.state.compare_exchange(
                state.into(),
                IoWakerState {
                    stored: true,
                    ..state
                }
                .into(),
                Ordering::AcqRel,
                Ordering::Acquire,
            ) {
                Ok(_) => {
                    begin_wait();
                    return Poll::Pending;
                }
                Err(new_state) => {
                    state = new_state.into();
                    assert!(state.notified);
                }
            }

            // Remove & drop the waker we cloned above
            let waker = mem::replace(&mut *slot_ptr, None);
            mem::drop(waker.unwrap());
            return Poll::Ready(state.epoch);
        }

        match self.state.compare_exchange(
            state.into(),
            IoWakerState {
                stored: false,
                ..state
            }
            .into(),
            Ordering::Acquire,
            Ordering::Acquire,
        ) {
            Ok(new_state) => state = new_state.into(),
            Err(new_state) => {
                state = new_state.into();
                assert!(state.notified);
                assert!(!state.stored);
                return Poll::Ready(state.epoch);
            }
        }

        let slot_ptr = self.wakers[state.slot as usize].get();
        let will_wake = (&*slot_ptr)
            .as_ref()
            .map(|waker| waker_ref.will_wake(waker))
            .expect("IoWaker was stored without a Waker");

        if !will_wake {
            let waker = mem::replace(&mut *slot_ptr, Some(waker_ref.clone()));
            mem::drop(waker.unwrap());
        }

        match self.state.compare_exchange(
            state.into(),
            IoWakerState {
                stored: true,
                ..state
            }
            .into(),
            Ordering::AcqRel,
            Ordering::Acquire,
        ) {
            Ok(_) => return Poll::Pending,
            Err(new_state) => {
                state = new_state.into();
                assert!(state.notified);
                assert!(!state.stored);
            }
        }

        let waker = mem::replace(&mut *slot_ptr, None);
        mem::drop(waker.unwrap());
        cancel_wait();
        Poll::Ready(state.epoch)
    }

    unsafe fn detach(&self) -> bool {
        let mut state: IoWakerState = self.state.load(Ordering::Relaxed).into();
        if !state.stored || state.notified {
            return false;
        }

        if let Err(new_state) = self.state.compare_exchange(
            state.into(),
            IoWakerState {
                stored: false,
                ..state
            }
            .into(),
            Ordering::Acquire,
            Ordering::Relaxed,
        ) {
            state = new_state.into();
            assert!(state.notified);
            assert!(!state.stored);
            return false;
        }

        let slot_ptr = self.wakers[state.slot as usize].get();
        let waker = mem::replace(&mut *slot_ptr, None);
        let waker = waker.expect("IoWaker detach stored when empty");
        mem::drop(waker);
        true
    }

    unsafe fn wake(&self) -> bool {
        self.state
            .fetch_update(Ordering::AcqRel, Ordering::Relaxed, |state| {
                let mut state: IoWakerState = state.into();
                state.notified = true;
                state.epoch += 1;

                if state.stored {
                    state.slot = !state.slot;
                    state.stored = false;
                }

                Some(state.into())
            })
            .map(|state| {
                let state: IoWakerState = state.into();
                if !state.stored {
                    return false;
                }

                let slot_ptr = self.wakers[state.slot as usize].get();
                let waker = mem::replace(&mut *slot_ptr, None);
                let waker = waker.expect("IoWaker wake & stored when empty");

                waker.wake();
                true
            })
            .unwrap_or(false)
    }

    unsafe fn reset(&self, epoch: usize) {
        let state: IoWakerState = self.state.load(Ordering::Relaxed).into();
        assert!(state.notified);
        assert!(!state.stored);

        if state.epoch == epoch {
            let _ = self.state.compare_exchange(
                state.into(),
                IoWakerState {
                    notified: false,
                    ..state
                }
                .into(),
                Ordering::Relaxed,
                Ordering::Relaxed,
            );
        }
    }
}

struct IoNode {
    next: Cell<Option<NonNull<Self>>>,
    wakers: [IoWaker; 2],
    _pinned: PhantomPinned,
}

impl IoNode {
    const fn new() -> Self {
        Self {
            next: Cell::new(None),
            wakers: [IoWaker::new(), IoWaker::new()],
            _pinned: PhantomPinned,
        }
    }
}

struct IoNodeBlock {
    _prev: Option<Pin<Box<Self>>>,
    nodes: [IoNode; Self::NODE_COUNT],
}

impl IoNodeBlock {
    const BLOCK_SIZE: usize = 64 * 1024;
    const HEADER_SIZE: usize = mem::size_of::<Option<Pin<Box<Self>>>>();

    const NODE_BYTES: usize = Self::BLOCK_SIZE - Self::HEADER_SIZE;
    const NODE_COUNT: usize = Self::NODE_BYTES / mem::size_of::<IoNode>();

    fn alloc(prev: Option<Pin<Box<Self>>>) -> Pin<Box<Self>> {
        unsafe {
            const EMPTY_NODE: IoNode = IoNode::new();

            let block = Pin::into_inner_unchecked(Box::pin(Self {
                _prev: prev, // keep a link to deallocate it
                nodes: [EMPTY_NODE; Self::NODE_COUNT],
            }));

            for index in 0..block.nodes.len() {
                block.nodes[index].next.set(match index + 1 {
                    Self::NODE_COUNT => None,
                    next_index => Some(NonNull::from(&block.nodes[next_index])),
                })
            }

            Pin::new_unchecked(block)
        }
    }
}

#[derive(Default)]
struct IoNodeCache {
    free_list: Option<NonNull<IoNode>>,
    blocks: Option<Pin<Box<IoNodeBlock>>>,
}

impl IoNodeCache {
    fn alloc(&mut self) -> NonNull<IoNode> {
        let node = self.free_list.unwrap_or_else(|| {
            let block = IoNodeBlock::alloc(self.blocks.take());
            let node = NonNull::from(&block.nodes[0]);
            self.blocks = Some(block);
            node
        });

        self.free_list = unsafe { node.as_ref().next.get() };
        node
    }

    unsafe fn recycle(&mut self, node: Pin<&IoNode>) {
        node.next.set(self.free_list);
        self.free_list = Some(NonNull::from(&*node));
    }
}

struct IoPoller {
    selector: mio::Poll,
    events: mio::event::Events,
}

impl Default for IoPoller {
    fn default() -> Self {
        Self {
            selector: mio::Poll::new().expect("failed to create I/O selector"),
            events: mio::event::Events::with_capacity(256),
        }
    }
}

impl IoPoller {
    fn poll(&mut self, notified: &mut bool, timeout: Option<Duration>) -> usize {
        if let Err(_) = self.selector.poll(&mut self.events, timeout) {
            return 0;
        }

        let mut resumed = 0;
        for event in &self.events {
            unsafe {
                let node = match NonNull::new(event.token().0 as *mut IoNode) {
                    Some(node_ptr) => Pin::new_unchecked(&*node_ptr.as_ptr()),
                    None => {
                        *notified = true;
                        continue;
                    }
                };

                if event.is_writable() || event.is_write_closed() || event.is_error() {
                    resumed += node.wakers[IoKind::Write as usize].wake() as usize;
                }
                if event.is_readable() || event.is_read_closed() || event.is_error() {
                    resumed += node.wakers[IoKind::Read as usize].wake() as usize;
                }
            }
        }
        
        resumed
    }
}

struct IoReactor {
    pending: AtomicUsize,
    waiting: AtomicBool,
    polling: AtomicBool,
    poller: UnsafeCell<IoPoller>,
    waker: mio::Waker,
    registry: mio::Registry,
    node_cache: Lock<IoNodeCache>,
}

impl Default for IoReactor {
    fn default() -> Self {
        let poller = IoPoller::default();

        let registry = poller
            .selector
            .registry()
            .try_clone()
            .expect("failed to clone I/O registry");

        let waker = mio::Waker::new(&registry, mio::Token(0))
            .expect("failed to create I/O notification Waker");

        Self {
            pending: AtomicUsize::new(0),
            waiting: AtomicBool::new(false),
            polling: AtomicBool::new(false),
            poller: UnsafeCell::new(poller),
            waker,
            registry,
            node_cache: Lock::new(IoNodeCache::default()),
        }
    }
}

impl IoReactor {
    fn notify(&self) -> bool {
        Self::try_transition_to(&self.waiting, false, Ordering::Release) && {
            self.waker
                .wake()
                .expect("failed to signal I/O notification Waker");
            true
        }
    }

    fn poll(&self, timeout: Option<Duration>) -> bool {
        self.try_with_poller(|poller| {
            let is_waiting = !matches!(timeout, Some(Duration::ZERO));
            if is_waiting {
                self.waiting.store(true, Ordering::Relaxed);
                std::sync::atomic::fence(Ordering::Acquire);
            }

            let mut notified = false;
            let mut resumed = poller.poll(&mut notified, timeout);

            if is_waiting {
                if !Self::try_transition_to(&self.waiting, false, Ordering::AcqRel) {
                    while !notified {
                        resumed += poller.poll(&mut notified, None);
                    }
                }
            } else {
                assert!(!notified);
            }

            if resumed > 0 {
                self.pending.fetch_sub(resumed, Ordering::Relaxed);
            }
        })
        .is_some()
    }

    fn try_with_poller<T>(&self, f: impl FnOnce(&mut IoPoller) -> T) -> Option<T> {
        if self.pending.load(Ordering::Acquire) == 0 {
            return None;
        }

        if !Self::try_transition_to(&self.polling, true, Ordering::Acquire) {
            return None;
        }

        let result = f(unsafe { &mut *self.poller.get() });
        self.polling.store(false, Ordering::Release);
        Some(result)
    }

    #[inline]
    fn try_transition_to(active: &AtomicBool, new_state: bool, order: Ordering) -> bool {
        if active.load(Ordering::Relaxed) == new_state {
            return false;
        }

        #[cfg(any(target_arch = "x86", target_arch = "x86_64"))]
        {
            active.swap(new_state, order) != new_state
        }

        #[cfg(not(any(target_arch = "x86", target_arch = "x86_64")))]
        {
            active
                .compare_exchange(!new_state, new_state, order, Ordering::Relaxed)
                .is_ok()
        }
    }
}

pub struct IoDriver {
    once: Once,
    reactor_init: AtomicBool,
    reactor: UnsafeCell<Option<IoReactor>>,
}

unsafe impl Send for IoDriver {}
unsafe impl Sync for IoDriver {}

impl Default for IoDriver {
    fn default() -> Self {
        Self {
            once: Once::new(),
            reactor_init: AtomicBool::new(false),
            reactor: UnsafeCell::new(None),
        }
    }
}

impl IoDriver {
    pub fn notify(&self) -> bool {
        self.try_get_reactor()
            .map(|reactor| reactor.notify())
            .unwrap_or(false)
    }

    pub fn poll(&self, timeout: Option<Duration>) -> bool {
        self.try_get_reactor()
            .map(|reactor| reactor.poll(timeout))
            .unwrap_or(false)
    }

    fn get_reactor(&self) -> &IoReactor {
        unsafe {
            self.once.call_once(|| {
                *self.reactor.get() = Some(IoReactor::default());
                self.reactor_init.store(true, Ordering::Release);
            });
            self.try_get_reactor().unwrap()
        }
    }

    fn try_get_reactor(&self) -> Option<&IoReactor> {
        if self.reactor_init.load(Ordering::Acquire) {
            Some(unsafe { (&*self.reactor.get()).as_ref().unwrap() })
        } else {
            None
        }
    }
}

pub struct IoSource<S: mio::event::Source> {
    source: S,
    node: NonNull<IoNode>,
    driver: Arc<IoDriver>,
}

impl<S: mio::event::Source> IoSource<S> {
    pub fn new(mut source: S, driver: Arc<IoDriver>) -> Self {
        let reactor = driver.get_reactor();
        let node = reactor.node_cache.with(|cache| cache.alloc());

        reactor
            .registry
            .register(
                &mut source,
                mio::Token(node.as_ptr() as usize),
                mio::Interest::READABLE | mio::Interest::WRITABLE,
            )
            .expect("failed to register IoSource to I/O registry");

        Self {
            source,
            node,
            driver,
        }
    }

    pub unsafe fn poll_io<T>(
        &self,
        kind: IoKind,
        waker_ref: &Waker,
        mut do_io: impl FnMut() -> io::Result<T>,
    ) -> Poll<io::Result<T>> {
        loop {
            let io_token = match self.node.as_ref().wakers[kind as usize].poll(
                waker_ref,
                || self.begin_wait(),
                || self.cancel_wait(),
            ) {
                Poll::Pending => return Poll::Pending,
                Poll::Ready(epoch) => epoch,
            };

            loop {
                match do_io() {
                    Err(e) if e.kind() == io::ErrorKind::Interrupted => continue,
                    Err(e) if e.kind() == io::ErrorKind::WouldBlock => {
                        self.node.as_ref().wakers[kind as usize].reset(io_token);
                        break;
                    }
                    result => return Poll::Ready(result),
                }
            }
        }
    }

    #[cold]
    pub unsafe fn detach_io(&self, kind: IoKind) {
        if self.node.as_ref().wakers[kind as usize].detach() {
            self.cancel_wait();
        }
    }

    pub unsafe fn wait_for<'a>(&'a self, kind: IoKind) -> impl Future<Output = ()> + 'a {
        struct WaitFor<'a, S: mio::event::Source> {
            source: Option<&'a IoSource<S>>,
            kind: IoKind,
        }

        impl<'a, S: mio::event::Source> Drop for WaitFor<'a, S> {
            fn drop(&mut self) {
                if let Some(source) = self.source.take() {
                    unsafe { source.detach_io(self.kind) };
                }
            }
        }

        impl<'a, S: mio::event::Source> Future for WaitFor<'a, S> {
            type Output = ();

            fn poll(mut self: Pin<&mut Self>, ctx: &mut Context<'_>) -> Poll<Self::Output> {
                let source = self
                    .source
                    .take()
                    .expect("WaitFor future polled after completion");

                match unsafe { source.poll_io(self.kind, ctx.waker(), || Ok(())) } {
                    Poll::Ready(_) => Poll::Ready(()),
                    Poll::Pending => {
                        self.source = Some(source);
                        Poll::Pending
                    }
                }
            }
        }

        WaitFor {
            source: Some(self),
            kind,
        }
    }

    #[cold]
    fn begin_wait(&self) {
        self.driver
            .get_reactor()
            .pending
            .fetch_add(1, Ordering::Relaxed);
    }

    #[cold]
    fn cancel_wait(&self) {
        self.driver
            .get_reactor()
            .pending
            .fetch_sub(1, Ordering::Relaxed);
    }
}

impl<S: mio::event::Source> AsRef<S> for IoSource<S> {
    fn as_ref(&self) -> &S {
        &self.source
    }
}

impl<S: mio::event::Source> Drop for IoSource<S> {
    fn drop(&mut self) {
        unsafe {
            let resumed = self
                .node
                .as_ref()
                .wakers
                .iter()
                .map(|waker| waker.detach() as usize)
                .sum();

            let reactor = self
                .driver
                .try_get_reactor()
                .expect("IoSource created without a Reactor");

            if resumed > 0 {
                reactor.pending.fetch_sub(resumed, Ordering::Relaxed);
            }

            reactor
                .registry
                .deregister(&mut self.source)
                .expect("failed to deregister IoSource");

            reactor.node_cache.with(|cache| {
                let node = Pin::new_unchecked(self.node.as_ref());
                cache.recycle(node)
            });
        }
    }
}

#[derive(Copy, Clone, Debug)]
struct IoFairState {
    tick: u8,
    be_fair: bool,
}

impl Into<u8> for IoFairState {
    fn into(self) -> u8 {
        (self.tick & (u8::MAX >> 1)) | (self.be_fair as u8)
    }
}

impl From<u8> for IoFairState {
    fn from(value: u8) -> Self {
        Self {
            tick: value >> 1,
            be_fair: value & 0b1 != 0,
        }
    }
}

#[derive(Default)]
pub struct IoFairness {
    state: u8,
}

impl IoFairness {
    pub unsafe fn poll_io<T, S: mio::event::Source>(
        &mut self,
        source: &IoSource<S>,
        kind: IoKind,
        waker_ref: &Waker,
        do_io: impl FnMut() -> io::Result<T>,
    ) -> Poll<io::Result<T>> {
        self.poll_fair(waker_ref, || source.poll_io(kind, waker_ref, do_io))
    }

    pub unsafe fn poll_fair<T>(
        &mut self,
        waker_ref: &Waker,
        poll_io: impl FnOnce() -> Poll<io::Result<T>>,
    ) -> Poll<io::Result<T>> {
        let mut state: IoFairState = self.state.into();

        if state.be_fair {
            state.be_fair = false;
            self.state = state.into();

            waker_ref.wake_by_ref();
            return Poll::Pending;
        }

        match poll_io() {
            Poll::Pending => Poll::Pending,
            Poll::Ready(result) => {
                state.be_fair = state.tick == 0;
                state.tick += 1;
                self.state = state.into();

                Poll::Ready(result)
            }
        }
    }
}
