use std::{
    cell::{Cell, UnsafeCell},
    future::Future,
    io,
    marker::PhantomPinned,
    mem,
    pin::Pin,
    ptr::{self, NonNull},
    sync::{
        atomic::{AtomicPtr, AtomicUsize, Ordering},
        Arc, Mutex,
    },
    task::{Context, Poll, Waker},
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

unsafe impl Send for IoNodeCache {}

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
            events: mio::event::Events::with_capacity(1024),
        }
    }
}

impl IoPoller {
    fn poll(&mut self) {
        loop {
            match self.selector.poll(&mut self.events, None) {
                Err(e) if e.kind() == io::ErrorKind::Interrupted => continue,
                Err(e) => unreachable!("I/O selector failed to poll: {:?}", e),
                Ok(_) => {}
            }

            for event in &self.events {
                unsafe {
                    let node = match NonNull::new(event.token().0 as *mut IoNode) {
                        Some(node_ptr) => Pin::new_unchecked(&*node_ptr.as_ptr()),
                        None => return,
                    };

                    if event.is_writable() || event.is_write_closed() || event.is_error() {
                        node.wakers[IoKind::Write as usize].wake();
                    }
                    if event.is_readable() || event.is_read_closed() || event.is_error() {
                        node.wakers[IoKind::Read as usize].wake();
                    }
                }
            }
        }
    }
}

pub struct IoDriver {
    poller: AtomicPtr<IoPoller>,
    waker: mio::Waker,
    registry: mio::Registry,
    node_cache: Mutex<IoNodeCache>,
}

impl Default for IoDriver {
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
            poller: AtomicPtr::new(Box::into_raw(Box::new(poller))),
            waker,
            registry,
            node_cache: Mutex::new(IoNodeCache::default()),
        }
    }
}

impl Drop for IoDriver {
    fn drop(&mut self) {
        mem::drop(self.take_poller());
    }
}

impl IoDriver {
    fn take_poller(&self) -> Option<Box<IoPoller>> {
        let poller = self.poller.swap(ptr::null_mut(), Ordering::Acquire);
        NonNull::new(poller).map(|p| unsafe { Box::from_raw(p.as_ptr()) })
    }

    pub fn poll(&self) {
        self.take_poller().map(|mut poller| poller.poll());
    }

    pub fn shutdown(&self) {
        self.waker.wake().expect("failed to shutdown I/O driver");
    }
}

pub struct IoSource<S: mio::event::Source> {
    source: S,
    node: NonNull<IoNode>,
    driver: Arc<IoDriver>,
}

impl<S: mio::event::Source> IoSource<S> {
    pub fn new(mut source: S, driver: Arc<IoDriver>) -> Self {
        let node = driver.node_cache.lock().unwrap().alloc();

        driver
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
            let io_waker = &self.node.as_ref().wakers[kind as usize];
            let io_token = match io_waker.poll(waker_ref, || {}, || {}) {
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
        self.node.as_ref().wakers[kind as usize].detach();
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
}

impl<S: mio::event::Source> AsRef<S> for IoSource<S> {
    fn as_ref(&self) -> &S {
        &self.source
    }
}

impl<S: mio::event::Source> Drop for IoSource<S> {
    fn drop(&mut self) {
        unsafe {
            let node = Pin::new_unchecked(self.node.as_ref());
            for waker in node.wakers.iter() {
                waker.detach();
            }

            self.driver
                .registry
                .deregister(&mut self.source)
                .expect("failed to deregister IoSource");

            self.driver
                .node_cache
                .lock()
                .unwrap()
                .recycle(Pin::new_unchecked(self.node.as_ref()));
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
