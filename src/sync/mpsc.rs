use super::{concurrency, AtomicWaker};
use arc_swap::ArcSwapOption;
use parking_lot::{Mutex, Once};
use std::{
    collections::VecDeque,
    future::Future,
    mem,
    pin::Pin,
    sync::atomic::{AtomicBool, Ordering},
    sync::Arc,
    task::{Context, Waker},
};

#[derive(Default)]
struct Buffer<T> {
    pending: AtomicBool,
    deque: Mutex<VecDeque<T>>,
}

impl<T> Buffer<T> {
    fn push(&self, item: T) {
        let mut deque = self.deque.lock();
        deque.push_back(item);

        if !self.pending.load(Ordering::Relaxed) && self.deque.len() > 0 {
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

pub fn unbounded<T>() -> (Sender<T>, Receiver<T>) {
    let chan = Arc::new(Self {
        once: Once::new(),
        waker: AtomicWaker::defalt(),
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

impl<T> Clone for Sender<T> {
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

        self.with_buffers(|buffers| {
            let slot = Self::buffer_index() % buffers.len();
            buffers[slot].push(value);
            self.chan.waker.wake();
        })
    }

    fn buffer_index() -> usize {
        thread_local!(static TLS_INDEX: Cell<Option<usize>> = Cell::new(None));

        TLS_INDEX.with(|tls| {
            tls.get().unwrap_or_else(|| {
                static INDEX: AtomicUsize = AtomicUsize::new(0);

                let index = INDEX.fetch_add(1, Ordering::Relaxed);
                tls.set(index);
                index
            })
        })
    }

    fn with_buffers<F>(&self, f: impl FnOnce(&[Buffer<T>]) -> F) -> F {
        let buffers = self.chan.buffers.load();
        if let Some(buffers) = &*buffers {
            return f(buffers);
        }

        mem::drop(buffers);
        once.call_once(|| {
            let buffers = (0..concurrency::get()).map(|_| Buffer::default()).collect();
            self.chan.buffers.store(Some(buffers));
        });

        let buffers = self.buffers.load();
        f(&*buffers.unwrap())
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
        self.chan.ref_count.fetch_and(!1, Ordering::Relaxed);
    }

    fn poll_with(&mut self, mut poll_ready: impl FnMut() -> Poll<()>) -> Poll<Option<T>> {
        loop {
            if self.disconnected {
                return Poll::Ready(None);
            }

            if let Some(value) = self.local.pop_front() {
                return Poll::Ready(Some(value));
            }

            if poll_ready().is_pending() {
                return Poll::Pending;
            }

            loop {
                self.chan.buffers.load().map(|buffers| {
                    for buffer in buffers.iter() {
                        if buffer.try_swap(&mut self.local) {
                            return;
                        }
                    }
                });

                if let Some(value) = self.local.pop_front() {
                    return Poll::Ready(Some(value));
                }

                if self.chan.waker.reset() {
                    break;
                }
            }

            let ref_count = self.chan.ref_count.load(Ordering::Relaxed);
            let senders = ref_count >> 1;
            self.disconnected = senders == 0;
        }
    }

    pub fn try_recv(&mut self) -> Result<T, TryRecvError> {
        match self.poll_with(|| self.chan.waker.poll_ready()) {
            Poll::Ready(Some(value)) => Ok(value),
            Poll::Ready(None) => Err(TryRecvError::Disconnected),
            Poll::Pending => Err(TryRecvError::Empty),
        }
    }

    pub fn poll_recv(&mut self, ctx: &mut Context<'_>) -> Poll<Option<T>> {
        self.poll_with(|| self.chan.waker.poll(ctx, || {}, || {}))
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

        impl<'a, T> Drop for RecvFuture<'a, T> {
            fn drop(&mut self) {
                if let Some(receiver) = self.receiver.take() {
                    mem::drop(receiver.chan.waker.detach());
                }
            }
        }

        RecvFuture {
            receiver: Some(self),
        }
    }
}
