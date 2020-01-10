use super::Event;
use core::{
    ptr::null,
    cell::Cell,
    mem::align_of,
    marker::PhantomData,
    sync::atomic::{fence, spin_loop_hint, Ordering, AtomicUsize},
};

const MUTEX_LOCK: usize = 1 << 0;
const QUEUE_LOCK: usize = 1 << 1;
const QUEUE_MASK: usize = !(MUTEX_LOCK | QUEUE_LOCK);

struct QueueNode<E> {
    prev: Cell<*const Self>,
    next: Cell<*const Self>,
    tail: Cell<*const Self>,
    event: E,
}

/// WordLock from https://github.com/Amanieu/parking_lot/blob/master/core/src/word_lock.rs
pub struct WordLock<E> {
    state: AtomicUsize,
    phantom: PhantomData<E>,
}

#[cfg(feature = "os")]
pub type Mutex<T> = RawMutex<T, super::OsEvent>;
#[cfg(feature = "os")]
pub type MutexGuard<'a, T> = RawMutexGuard<'a, T, super::OsEvent>;

pub type RawMutex<T, E> = lock_api::Mutex<WordLock<E>, T>;
pub type RawMutexGuard<'a, T, E> = lock_api::MutexGuard<'a, WordLock<E>, T>;

unsafe impl<E: Event> lock_api::RawMutex for WordLock<E> {
    const INIT: Self = Self::new();

    type GuardMarker = lock_api::GuardSend;

    fn try_lock(&self) -> bool {
        self.state
            .compare_exchange_weak(0, MUTEX_LOCK, Ordering::Acquire, Ordering::Relaxed)
            .is_ok()
    }

    fn lock(&self) {
        if !self.try_lock() {
            self.lock_slow();
        }
    }

    fn unlock(&self) {
        let state = self.state.fetch_sub(MUTEX_LOCK, Ordering::Release);
        if (state & QUEUE_LOCK == 0) && (state & QUEUE_MASK != 0) {
            unsafe { self.unlock_slow() };
        }
    }
}

impl<E> WordLock<E> {
    pub const fn new() -> Self {
        Self {
            state: AtomicUsize::new(0),
            phantom: PhantomData,
        }
    }
}

impl<E: Event> WordLock<E> {
    #[cold]
    fn lock_slow(&self) {
        const SPIN_COUNT_DOUBLING: usize = 4;
        
        // try to lock the mutex before allocating the node on the stack
        // as that may be potentially expensive due to the Event implementation.
        let mut spin = 0;
        let mut state = self.state.load(Ordering::Relaxed);
        loop {
            if state & MUTEX_LOCK == 0 {
                match self.state.compare_exchange_weak(
                    state,
                    state | MUTEX_LOCK,
                    Ordering::Acquire,
                    Ordering::Relaxed,
                ) {
                    Ok(_) => return,
                    Err(s) => state = s,
                }
            } else if (state & QUEUE_MASK == 0) && spin < SPIN_COUNT_DOUBLING {
                spin += 1;
                (0..(1 << spin)).for_each(|_| spin_loop_hint());
                state = self.state.load(Ordering::Relaxed);
            } else {
                break;
            }
        }
        
        assert!(align_of::<QueueNode<E>>() > !QUEUE_MASK);
        let mut node = QueueNode {
            prev: Cell::new(null()),
            next: Cell::new(null()),
            tail: Cell::new(null()),
            event: E::default(),
        };

        loop {
            if state & MUTEX_LOCK == 0 {
                match self.state.compare_exchange_weak(
                    state,
                    state | MUTEX_LOCK,
                    Ordering::Acquire,
                    Ordering::Relaxed,
                ) {
                    Ok(_) => return,
                    Err(s) => state = s,
                }
                continue;
            }

            if (state & QUEUE_MASK == 0) && spin < SPIN_COUNT_DOUBLING {
                spin += 1;
                (0..(1 << spin)).for_each(|_| spin_loop_hint());
                state = self.state.load(Ordering::Relaxed);
                continue;
            }
            
            let head = (state & QUEUE_MASK) as *const QueueNode<E>;
            if head.is_null() {
                node.tail.set(&node);
                node.next.set(null());
            } else {
                node.tail.set(null());
                node.next.set(head);
            }

            if let Err(s) = self.state.compare_exchange_weak(
                state,
                (&node as *const _ as usize) | (state & !QUEUE_MASK),
                Ordering::Release,
                Ordering::Relaxed,
            ) {
                state = s;
                continue;
            }

            node.event.wait();
            node.event.reset();
            node.prev.set(null());
            spin = 0;
            state = self.state.load(Ordering::Relaxed);
        }
    }

    #[cold]
    unsafe fn unlock_slow(&self) {
        let mut state = self.state.load(Ordering::Relaxed);
        loop {
            if (state & QUEUE_LOCK != 0) || (state & QUEUE_MASK == 0) {
                return;
            }
            match self.state.compare_exchange_weak(
                state,
                state | QUEUE_LOCK,
                Ordering::Acquire,
                Ordering::Relaxed,
            ) {
                Ok(_) => break,
                Err(s) => state = s,
            }
        }

        'outer: loop {
            let head = &*((state & QUEUE_MASK) as *const QueueNode<E>);
            let mut current = head;
            while current.tail.get().is_null() {
                let next = &*current.next.get();
                next.prev.set(current);
                current = next;
            }
            let tail = &*current.tail.get();
            head.tail.set(tail);

            if state & MUTEX_LOCK != 0 {
                match self.state.compare_exchange_weak(
                    state,
                    state & !QUEUE_LOCK,
                    Ordering::Release,
                    Ordering::Relaxed,
                ) {
                    Ok(_) => return,
                    Err(s) => state = s,
                }
                fence(Ordering::Acquire);
                continue;
            }

            let new_tail = tail.prev.get();
            if new_tail.is_null() {
                loop {
                    match self.state.compare_exchange_weak(
                        state,
                        state & MUTEX_LOCK,
                        Ordering::Release,
                        Ordering::Relaxed,
                    ) {
                        Ok(_) => break,
                        Err(s) => state = s,
                    }
                    if state & QUEUE_MASK != 0 {
                        fence(Ordering::Acquire);
                        continue 'outer;
                    }
                }
            } else {
                head.tail.set(new_tail);
                self.state.fetch_and(!QUEUE_LOCK, Ordering::Release);
            }

            tail.event.set();
            return;
        }
    }
}
