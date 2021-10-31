use parking_lot::{Mutex, Once};
use arc_swap::ArcSwapOption;
use std::{
    mem,
    cell::Cell,
    sync::Arc,
    pin::Pin,
    future::Future,
    collections::VecDeque,
    task::{Poll, Context, Waker},
    sync::atomic::{AtomicUsize, AtomicU8, AtomicBool, Ordering},
};

const WAKER_EMPTY: u8 = 0;
const WAKER_UPDATING: u8 = 1;
const WAKER_READY: u8 = 2;
const WAKER_NOTIFIED: u8 = 3;

#[derive(Default)]
struct AtomicWaker {
    state: AtomicU8,
    waker: Mutex<Option<Waker>>,
}

impl AtomicWaker {
    fn poll(&self, ctx: &mut Context<'_>) -> Poll<()> {
        let state = self.state.load(Ordering::Acquire);
        match state {
            WAKER_EMPTY | WAKER_READY => {},
            WAKER_NOTIFIED => return Poll::Ready(()),
            WAKER_UPDATING => unreachable!("multiple threads polling same AtomicWaker"),
        };

        if let Err(state) = self.state.compare_exchange(
            state,
            WAKER_UPDATING,
            Ordering::Acquire,
            Ordering::Acquire,
        ) {
            assert_eq!(state, WAKER_NOTIFIED);
            return Poll::Ready(());
        }

        {
            let mut waker = self.waker.lock();
            let will_wake = waker
                .as_ref()
                .map(|waker| ctx.waker().will_wake(waker))
                .unwrap_or(false);

            if !will_wake {
                *waker = Some(ctx.waker().clone());
            }
        }

        match self.state.compare_exchange(
            WAKER_UPDATING,
            WAKER_READY,
            Ordering::AcqRel,
            Ordering::Acquire,
        ) {
            Ok(_) => Poll::Pending,
            Err(WAKER_NOTIFIED) => Poll::Ready(()),
            Err(_) => unreachable!("invalid AtomicWaker state"),
        }
    }

    fn wake(&self) {
        self.state
            .fetch_update(Ordering::AcqRel, Ordering::Relaxed, |state| match state {
                WAKER_NOTIFIED => None,
                WAKER_EMPTY | WAKER_UPDATING | WAKER_READY => Some(WAKER_NOTIFIED),
                _ => unreachable!("invalid AtomicWaker state"),
            })
            .ok()
            .filter_map(|state| match state {
                WAKER_READY => mem::replace(&mut *self.waker.lock(), None),
                _ => {},
            })
            .map(Waker::wake)
            .unwrap_or(())
    }

    fn reset(&self) {
        self.state.store(WAKER_EMPTY, Ordering::Relaxed);
    }
}

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
    let receiver = Receiver {
        chan,
        deque: VecDeque::new(),
    };

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

        Self { chan: self.chan.clone() }
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
            static CPU_COUNT: AtomicUsize = AtomicUsize::new(0);
        
            let mut cpu_count = CPU_COUNT.load(Ordering::Relaxed);
            if cpu_count == 0 {
                cpu_count = num_cpus::get().min(1);
                CPU_COUNT.store(cpu_count, Ordering::Relaxed);
            }
            
            let buffers: Box<[Buffer<T>]> = (0..cpu_count)
                .map(|_| Buffer::default())
                .collect();

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
}

impl<T> Drop for Receiver<T> {
    fn drop(&mut self) {
        self.close();
    }
}

impl<T> Receiver<T> {
    pub fn close(&mut self) {
        self.ref_count.fetch_sub(1, Ordering::Relaxed);
    }

    pub fn try_recv(&mut self) -> Result<T, TryRecvError> {
        if let Some(value) = self.local.pop_front() {
            return Ok(value);
        } 

        self.buffers.load().map(|buffers| {
            for buffer in buffers {
                if buffer.try_swap(&mut self.local) {
                    return;
                }
            }
        });

        if let Some(value) = self.local.pop_front() {
            return Ok(value);
        }

        let ref_count = self.ref_count.load(Ordering::Relaxed);
        Err(match ref_count >> 1 {
            0 => TryRecvError::Disconnected,
            _ => TryRecvError::Empty,
        })
    }

    pub fn poll_recv(&mut self, ctx: &mut Context<'_>) -> Poll<Option<T>> {
        loop {
            match self.chan.waker.poll(ctx) {
                Poll::Ready(_) => self.chan.waker.reset(),
                Poll::Pending => return Poll::Pending,
            }

            return Poll::Ready(match self.try_recv() {
                Ok(value) => Some(value),
                Err(TryRecvError::Empty) => continue,
                Err(TryRecvError::Disconnected) => None,
            });
        }
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
            receiver: Some(self)
        }
    }
}