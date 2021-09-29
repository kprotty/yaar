use super::{
    super::sync::low_level::{AtomicWaker, Lock, Once, WakerUpdate},
    pool::Pool,
};
use std::{
    cell::{Cell, UnsafeCell},
    future::Future,
    io,
    marker::PhantomPinned,
    mem,
    pin::Pin,
    ptr::NonNull,
    sync::{
        atomic::{AtomicBool, AtomicU8, AtomicIsize, Ordering},
        Arc,
    },
    task::{Context, Poll, Waker},
    time::Duration,
};

struct IoWaker {
    waker: AtomicWaker,
    blocking: Cell<bool>,
}

impl IoWaker {
    const fn new() -> Self {
        Self {
            waker: AtomicWaker::new(),
            blocking: Cell::new(false),
        }
    }
}

struct IoNode {
    next: Cell<Option<NonNull<Self>>>,
    interest: Cell<Option<mio::Interest>>,
    reader: IoWaker,
    writer: IoWaker,
    _pinned: PhantomPinned,
}

impl IoNode {
    const fn new() -> Self {
        Self {
            next: Cell::new(None),
            interest: Cell::new(None),
            reader: IoWaker::new(),
            writer: IoWaker::new(),
            _pinned: PhantomPinned,
        }
    }
}

#[derive(Default)]
struct IoNodeList {
    top: Option<NonNull<IoNode>>,
}

impl IoNodeList {
    pub unsafe fn push(&mut self, node: NonNull<IoNode>) {
        node.as_ref().next.set(self.top);
        self.top = Some(node);
    }
}

impl Iterator for IoNodeList {
    type Item = NonNull<IoNode>;

    fn next(&mut self) -> Option<Self::Item> {
        let node = self.top?;
        self.top = unsafe { node.as_ref().next.get() };
        Some(node)
    }
}

struct IoNodeBlock {
    _next: Cell<Option<Pin<Box<Self>>>>,
    nodes: [IoNode; Self::BLOCK_COUNT],
}

impl IoNodeBlock {
    const BLOCK_HEADER: usize = mem::size_of::<Cell<Option<Pin<Box<Self>>>>>();
    const BLOCK_COUNT: usize = ((64 * 1024) - Self::BLOCK_HEADER) / mem::size_of::<IoNode>();
}

impl IoNodeBlock {
    unsafe fn alloc(next: Option<Pin<Box<Self>>>) -> Pin<Box<Self>> {
        const EMPTY_NODE: IoNode = IoNode::new();

        let block = Pin::into_inner_unchecked(Box::pin(Self {
            _next: Cell::new(next),
            nodes: [EMPTY_NODE; Self::BLOCK_COUNT],
        }));

        for index in 0..Self::BLOCK_COUNT {
            block.nodes[index].next.set(match index + 1 {
                Self::BLOCK_COUNT => None,
                next => Some(NonNull::from(&block.nodes[next])),
            });
        }

        Pin::new_unchecked(block)
    }
}

#[derive(Default)]
struct IoNodeCache {
    stack: Option<NonNull<IoNode>>,
    blocks: Option<Pin<Box<IoNodeBlock>>>,
}

unsafe impl Send for IoNodeCache {}
unsafe impl Sync for IoNodeCache {}

impl IoNodeCache {
    fn alloc(&mut self) -> NonNull<IoNode> {
        unsafe {
            let node = self.stack.unwrap_or_else(|| {
                let block = IoNodeBlock::alloc(self.blocks.take());
                let block = Pin::into_inner_unchecked(block);
                let node = NonNull::from(&block.nodes[0]);

                self.blocks = Some(Pin::new_unchecked(block));
                node
            });

            self.stack = node.as_ref().next.get();
            node
        }
    }

    unsafe fn dealloc(&mut self, node: NonNull<IoNode>) {
        node.as_ref().next.set(self.stack);
        self.stack = Some(node);
    }
}

struct IoPoller {
    io_poll: mio::Poll,
    io_events: mio::Events,
}

impl Default for IoPoller {
    fn default() -> Self {
        Self {
            io_poll: mio::Poll::new().expect("failed to create os poller"),
            io_events: mio::Events::with_capacity(1024),
        }
    }
}

impl IoPoller {
    pub fn poll(
        &mut self,
        node_list: &mut IoNodeList,
        notified: &mut bool,
        timeout: Option<Duration>,
    ) {
        if let Err(_) = self.io_poll.poll(&mut self.io_events, timeout) {
            return;
        }

        for event in &self.io_events {
            let node = match NonNull::new(event.token().0 as *mut IoNode) {
                Some(node) => node,
                None => {
                    *notified = true;
                    continue;
                }
            };

            let mut interest = None;
            let readable = mio::Interest::READABLE;
            let writable = mio::Interest::WRITABLE;

            if event.is_writable() || event.is_write_closed() || event.is_error() {
                interest = Some(interest.unwrap_or(writable) | writable);
            }
            if event.is_readable() || event.is_read_closed() || event.is_error() {
                interest = Some(interest.unwrap_or(readable) | readable);
            }

            unsafe {
                node.as_ref().interest.set(interest);
                node_list.push(node);
            }
        }
    }
}

#[derive(Copy, Clone, Debug, Eq, PartialEq)]
enum IoState {
    Empty = 0,
    Waiting = 1,
    Notified = 2,
}

impl Into<u8> for IoState {
    fn into(self) -> u8 {
        self as u8
    }
}

impl From<u8> for IoState {
    fn from(value: u8) -> Self {
        match value {
            0 => Self::Empty,
            1 => Self::Waiting,
            2 => Self::Notified,
            _ => unreachable!("invalid IoState value"),
        }
    }
}

struct IoDriverInner {
    io_state: AtomicU8,
    io_waker: mio::Waker,
    io_registry: mio::Registry,
    io_polling: AtomicBool,
    io_poller: UnsafeCell<IoPoller>,
    io_node_cache: Lock<IoNodeCache>,
}

unsafe impl Send for IoDriverInner {}
unsafe impl Sync for IoDriverInner {}

impl Default for IoDriverInner {
    fn default() -> Self {
        let io_poller = IoPoller::default();
        let io_registry = io_poller
            .io_poll
            .registry()
            .try_clone()
            .expect("failed to clone os-poll registration");

        let io_waker = mio::Waker::new(&io_registry, mio::Token(0))
            .expect("failed to create os notification waker");

        Self {
            io_state: AtomicU8::new(0),
            io_waker,
            io_registry,
            io_polling: AtomicBool::new(false),
            io_poller: UnsafeCell::new(io_poller),
            io_node_cache: Lock::new(IoNodeCache::default()),
        }
    }
}

impl IoDriverInner {
    fn notify(&self) -> bool {
        self.io_state
            .fetch_update(
                Ordering::Release,
                Ordering::Relaxed,
                |io_state| match IoState::from(io_state) {
                    IoState::Waiting => Some(IoState::Notified.into()),
                    _ => None,
                },
            )
            .map(IoState::from)
            .map(|io_state| {
                io_state == IoState::Waiting && {
                    self.io_waker.wake().expect("failed to wake up os notifier");
                    true
                }
            })
            .unwrap_or(false)
    }

    pub fn poll(&self, io_driver: &IoDriver, timeout: Option<Duration>) -> bool {
        self.try_with_poller(io_driver, |io_poller| {
            let mut notified = false;
            let mut node_list = IoNodeList::default();
            let is_blocking = !matches!(timeout, Some(Duration::ZERO));

            let mut io_state = self.io_state.load(Ordering::Acquire).into();
            assert_eq!(io_state, IoState::Empty);

            if is_blocking {
                let io_state = IoState::Waiting.into();
                self.io_state.store(io_state, Ordering::Relaxed);
            }

            io_poller.poll(&mut node_list, &mut notified, timeout);

            io_state = self.io_state.load(Ordering::Relaxed).into();
            if io_state == IoState::Waiting {
                match self.io_state.compare_exchange(
                    IoState::Waiting.into(),
                    IoState::Empty.into(),
                    Ordering::AcqRel,
                    Ordering::Acquire,
                ) {
                    Ok(_) => io_state = IoState::Empty,
                    Err(e) => io_state = IoState::from(e),
                }
            }

            if io_state == IoState::Notified {
                while !notified {
                    io_poller.poll(&mut node_list, &mut notified, None);
                }

                let new_io_state = IoState::Empty as u8;
                self.io_state.store(new_io_state, Ordering::Release);
            }

            node_list
        })
        .map(|node_list| {
            for node in node_list {
                let node = unsafe { node.as_ref() };
                let interest = node.interest.replace(None);
                
                if interest.map(|i| i.is_writable()).unwrap_or(false) {
                    if let Some(waker) = node.writer.waker.wake() {
                        io_driver.mark_io_end();
                        waker.wake();
                    }
                }

                if interest.map(|i| i.is_readable()).unwrap_or(false) {
                    if let Some(waker) = node.reader.waker.wake() {
                        io_driver.mark_io_end();
                        waker.wake();
                    }
                }
            }
        })
        .is_some()
    }

    fn try_with_poller<T>(&self, io_driver: &IoDriver, f: impl FnOnce(&mut IoPoller) -> T) -> Option<T> {
        if !io_driver.has_io_pending() {
            return None;
        }

        if self.io_polling.load(Ordering::Relaxed) {
            return None;
        }

        if self.io_polling.swap(true, Ordering::Acquire) {
            return None;
        }

        let result = f(unsafe { &mut *self.io_poller.get() });
        self.io_polling.store(false, Ordering::Release);
        Some(result)
    }
}

pub struct IoDriver {
    once: Once,
    pending: AtomicIsize,
    initialized: AtomicBool,
    inner: UnsafeCell<Option<IoDriverInner>>,
}

unsafe impl Send for IoDriver {}
unsafe impl Sync for IoDriver {}

impl IoDriver {
    pub fn new() -> Arc<IoDriver> {
        let io_driver = Arc::new(Self {
            once: Once::new(),
            pending: AtomicIsize::new(0),
            initialized: AtomicBool::new(false),
            inner: UnsafeCell::new(None),
        });

        let iod = io_driver.clone();
        std::thread::spawn(move || loop {
            let pending = iod.pending.load(Ordering::SeqCst);
            println!("pending: {}", pending);
            std::thread::sleep(std::time::Duration::from_secs(1));
        });

        io_driver
    }

    fn mark_io_begin(&self) {
        self.pending.fetch_add(1, Ordering::SeqCst);
    }

    fn mark_io_end(&self) {
        self.pending.fetch_sub(1, Ordering::SeqCst);
    }

    fn has_io_pending(&self) -> bool {
        self.pending.load(Ordering::SeqCst) > 0
    }

    pub fn notify(&self) -> bool {
        self.try_inner()
            .map(|inner| inner.notify())
            .unwrap_or(false)
    }

    pub fn poll(&self, timeout: Option<Duration>) -> bool {
        self.try_inner()
            .map(|inner| inner.poll(self, timeout))
            .unwrap_or(false)
    }

    fn try_inner(&self) -> Option<&IoDriverInner> {
        match self.initialized.load(Ordering::Acquire) {
            true => Some(unsafe { self.get_inner_unchecked() }),
            _ => None,
        }
    }

    fn get_inner(&self) -> &IoDriverInner {
        unsafe {
            self.once.call_once(|| {
                *self.inner.get() = Some(IoDriverInner::default());
                self.initialized.store(true, Ordering::Release);
            });
            self.get_inner_unchecked()
        }
    }

    unsafe fn get_inner_unchecked(&self) -> &IoDriverInner {
        (&*self.inner.get())
            .as_ref()
            .expect("data race: IoDriverInner not initialized")
    }
}

#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum IoKind {
    Read,
    Write,
}

pub struct IoSource<S: mio::event::Source> {
    io_source: S,
    io_node: NonNull<IoNode>,
    io_driver: Arc<IoDriver>,
}

unsafe impl<S: mio::event::Source + Send> Send for IoSource<S> {}
unsafe impl<S: mio::event::Source + Sync> Sync for IoSource<S> {}

impl<S: mio::event::Source> AsRef<S> for IoSource<S> {
    fn as_ref(&self) -> &S {
        &self.io_source
    }
}

impl<S: mio::event::Source> Drop for IoSource<S> {
    fn drop(&mut self) {
        unsafe {
            self.detach_io(IoKind::Read);
            self.detach_io(IoKind::Write);

            let inner = self
                .io_driver
                .try_inner()
                .expect("IoDriverInner uninitialized while IoSource exists");

            let _ = inner.io_registry.deregister(&mut self.io_source);
            inner.io_node_cache.with(|c| c.dealloc(self.io_node));
        }
    }
}

impl<S: mio::event::Source> IoSource<S> {
    pub fn new(mut source: S) -> Self {
        Pool::with_current(|pool, _index| {
            let inner = pool.io_driver.get_inner();
            let node = inner.io_node_cache.with(|cache| cache.alloc());

            inner
                .io_registry
                .register(
                    &mut source,
                    mio::Token(node.as_ptr() as usize),
                    mio::Interest::READABLE | mio::Interest::WRITABLE,
                )
                .expect("failed to register I/O source");

            Self {
                io_source: source,
                io_node: node,
                io_driver: pool.io_driver.clone(),
            }
        })
        .expect("IoSource::new() called outside the runtime")
    }

    pub unsafe fn poll_io<T>(
        &self,
        kind: IoKind,
        waker: &Waker,
        mut yield_now: impl FnMut() -> bool,
        mut do_io: impl FnMut() -> io::Result<T>,
    ) -> Poll<io::Result<T>> {
        let io_waker = match kind {
            IoKind::Read => &self.io_node.as_ref().reader,
            IoKind::Write => &self.io_node.as_ref().writer,
        };

        loop {
            if io_waker.blocking.get() {
                match io_waker.waker.register(waker) {
                    Err(None) => {
                        self.io_driver.mark_io_begin();
                        return Poll::Pending;
                    },
                    Err(Some(waker)) => {
                        mem::drop(waker);
                        return Poll::Pending;
                    },
                    Ok(Some(waker)) => {
                        self.io_driver.mark_io_end();
                        mem::drop(waker);
                    },
                    Ok(None) => {}
                }

                io_waker.blocking.set(false);
                io_waker.waker.reset();
            }

            if yield_now() {
                waker.wake_by_ref();
                return Poll::Pending;
            }

            loop {
                match do_io() {
                    Err(e) if e.kind() == io::ErrorKind::Interrupted => continue,
                    Err(e) if e.kind() == io::ErrorKind::WouldBlock => {
                        io_waker.blocking.set(true);
                        break;
                    }
                    result => return Poll::Ready(result),
                }
            }
        }
    }

    pub fn detach_io(&self, kind: IoKind) {
        let io_waker = unsafe {
            match kind {
                IoKind::Read => &self.io_node.as_ref().reader,
                IoKind::Write => &self.io_node.as_ref().writer,
            }
        };

        if let Some(waker) = io_waker.waker.detach() {
            self.io_driver.mark_io_end();
            mem::drop(waker);
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
                    source.detach_io(self.kind);
                }
            }
        }

        impl<'a, S: mio::event::Source> Future for WaitFor<'a, S> {
            type Output = ();

            fn poll(mut self: Pin<&mut Self>, ctx: &mut Context<'_>) -> Poll<Self::Output> {
                let source = self.source.expect("wait_for polled after completion");
                let polled = unsafe {
                    let do_io = || Ok(());
                    let yield_now = || false;
                    source.poll_io(self.kind, ctx.waker(), yield_now, do_io)
                };

                match polled {
                    Poll::Pending => Poll::Pending,
                    Poll::Ready(Err(_)) => unreachable!(),
                    Poll::Ready(Ok(_)) => {
                        self.source = None;
                        Poll::Ready(())
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

#[derive(Default)]
pub struct IoFairness {
    tick: u8,
}

impl IoFairness {
    pub unsafe fn poll_io<T, S: mio::event::Source>(
        &mut self,
        source: &IoSource<S>,
        kind: IoKind,
        waker: &Waker,
        do_io: impl FnMut() -> io::Result<T>,
    ) -> Poll<io::Result<T>> {
        let yield_now = || {
            let tick = self.tick;
            self.tick = tick.wrapping_add(1);
            match tick {
                0 => false,
                _ => tick % 128 == 0,
            }
        };

        source.poll_io(kind, waker, yield_now, do_io)
    }
}
