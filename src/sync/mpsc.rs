use super::{concurrency, AtomicWaker};
use arc_swap::ArcSwapOption;
use parking_lot::{Mutex, Once};
use std::{
    cell::Cell,
    collections::VecDeque,
    future::Future,
    mem,
    pin::Pin,
    sync::atomic::{AtomicBool, AtomicUsize, Ordering},
    sync::Arc,
    task::{Context, Poll, Waker},
};

#[derive(Default)]
struct Buffer<T> {
    pending: AtomicBool,
    deque: Mutex<VecDeque<T>>,
}

impl<T> Buffer<T> {
    fn new() -> Self {
        Self {
            pending: AtomicBool::new(false),
            deque: Mutex::new(VecDeque::new()),
        }
    }

    fn push(&self, item: T) {
        let mut deque = self.deque.lock();
        deque.push_back(item);

        if !self.pending.load(Ordering::Relaxed) && deque.len() > 0 {
            self.pending.store(true, Ordering::Relaxed);
        }
    }

    fn try_swap(&self, old_deque: &mut VecDeque<T>) -> bool {
        if !self.pending.load(Ordering::Relaxed) {
            return false;
        }

        let mut deque = match self.deque.try_lock() {
            Some(guard) => guard,
            None => return false,
        };

        if !self.pending.load(Ordering::Relaxed) {
            return false;
        }

        assert_eq!(old_deque.len(), 0);
        mem::swap(&mut *deque, old_deque);
        self.pending.store(false, Ordering::Relaxed);
        true
    }
}

struct Chan<T> {
    once: Once,
    waker: AtomicWaker,
    ref_count: AtomicUsize,
    buffers: ArcSwapOption<Box<[Buffer<T>]>>,
}

pub fn unbounded<T: Send>() -> (Sender<T>, Receiver<T>) {
    let chan = Arc::new(Chan {
        once: Once::new(),
        waker: AtomicWaker::default(),
        ref_count: AtomicUsize::new((1 << 1) | 1),
        buffers: ArcSwapOption::const_empty(),
    });

    let sender = Sender { chan: chan.clone() };
    let receiver = Receiver::new(chan);
    (sender, receiver)
}

pub struct SendError<T>(pub T);

pub struct Sender<T> {
    chan: Arc<Chan<T>>,
}

impl<T: Send> Clone for Sender<T> {
    fn clone(&self) -> Self {
        let ref_count = self.chan.ref_count.fetch_add(1 << 1, Ordering::Relaxed);
        assert_ne!(ref_count >> 1, usize::MAX >> 1);

        Self {
            chan: self.chan.clone(),
        }
    }
}

impl<T> Drop for Sender<T> {
    fn drop(&mut self) {
        let ref_count = self.chan.ref_count.fetch_sub(1 << 1, Ordering::Relaxed);
        assert_ne!(ref_count >> 1, 0);

        if ref_count == ((1 << 1) | 1) {
            self.chan.waker.wake();
        }
    }
}

impl<T> Sender<T> {
    pub fn send(&mut self, value: T) -> Result<(), SendError<T>> {
        let ref_count = self.chan.ref_count.load(Ordering::Relaxed);
        assert_ne!(ref_count >> 1, 0);

        if ref_count & 1 == 0 {
            return Err(SendError(value));
        }

        Ok(self.with_buffers(|buffers| {
            let slot = Self::buffer_index() % buffers.len();
            buffers[slot].push(value);
            self.chan.waker.wake().map(Waker::wake).unwrap_or(())
        }))
    }

    fn buffer_index() -> usize {
        thread_local!(static TLS_INDEX: Cell<Option<usize>> = Cell::new(None));

        TLS_INDEX.with(|tls| {
            tls.get().unwrap_or_else(|| {
                static INDEX: AtomicUsize = AtomicUsize::new(0);

                let index = INDEX.fetch_add(1, Ordering::Relaxed);
                tls.set(Some(index));
                index
            })
        })
    }

    fn with_buffers<F>(&self, f: impl FnOnce(&[Buffer<T>]) -> F) -> F {
        let buffers = self.chan.buffers.load();
        if let Some(buffers) = buffers.as_ref() {
            return f(buffers);
        }

        mem::drop(buffers);
        self.chan.once.call_once(|| {
            let buffer_count = concurrency::get().get();
            let buffers: Box<[Buffer<T>]> = (0..buffer_count).map(|_| Buffer::new()).collect();
            self.chan.buffers.store(Some(Arc::new(buffers)));
        });

        let buffers = self.chan.buffers.load();
        f(&*buffers.as_ref().unwrap())
    }
}

#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum TryRecvError {
    Empty,
    Disconnected,
}

pub struct Receiver<T> {
    chan: Arc<Chan<T>>,
    local: VecDeque<T>,
    disconnected: bool,
}

impl<T> Drop for Receiver<T> {
    fn drop(&mut self) {
        self.close();
    }
}

impl<T> Receiver<T> {
    fn new(chan: Arc<Chan<T>>) -> Self {
        Self {
            chan,
            local: VecDeque::new(),
            disconnected: false,
        }
    }

    pub fn close(&mut self) {
        let ref_count = self.chan.ref_count.load(Ordering::Relaxed);
        if ref_count & 1 != 0 {
            self.chan.ref_count.fetch_and(!1, Ordering::Relaxed);
        }
    }

    fn poll_recv_with(&mut self, waker: Option<&Waker>) -> Poll<Option<T>> {
        loop {
            if let Some(value) = self.local.pop_front() {
                return Poll::Ready(Some(value));
            }

            if self.disconnected {
                return Poll::Ready(None);
            }

            while let Poll::Ready(_) = self.chan.waker.poll(waker, || {}, || {}) {
                self.chan.buffers.load().as_ref().map(|buffers| {
                    for buffer in buffers.iter() {
                        if buffer.try_swap(&mut self.local) {
                            return;
                        }
                    }
                });

                match self.local.pop_front() {
                    Some(value) => return Poll::Ready(Some(value)),
                    None => self.chan.waker.reset(),
                }
            }

            let ref_count = self.chan.ref_count.load(Ordering::Relaxed);
            if (ref_count >> 1) > 0 {
                return Poll::Pending;
            }

            self.disconnected = true;
            return Poll::Ready(None);
        }
    }

    pub fn try_recv(&mut self) -> Result<T, TryRecvError> {
        match self.poll_recv_with(None) {
            Poll::Ready(Some(value)) => Ok(value),
            Poll::Ready(None) => Err(TryRecvError::Disconnected),
            Poll::Pending => Err(TryRecvError::Empty),
        }
    }

    pub fn poll_recv(&mut self, ctx: &mut Context<'_>) -> Poll<Option<T>> {
        self.poll_recv_with(Some(ctx.waker()))
    }

    pub fn recv<'a>(&'a mut self) -> impl Future<Output = Option<T>> + 'a {
        struct RecvFuture<'a, T> {
            receiver: Option<&'a mut Receiver<T>>,
        }

        impl<'a, T> Future for RecvFuture<'a, T> {
            type Output = Option<T>;

            fn poll(mut self: Pin<&mut Self>, ctx: &mut Context<'_>) -> Poll<Self::Output> {
                let receiver = self
                    .receiver
                    .take()
                    .expect("RecvFuture polled after completion");

                match receiver.poll_recv(ctx) {
                    Poll::Ready(result) => Poll::Ready(result),
                    Poll::Pending => {
                        self.receiver = Some(receiver);
                        Poll::Pending
                    }
                }
            }
        }

        RecvFuture {
            receiver: Some(self),
        }
    }
}
