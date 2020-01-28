use crate::ThreadEvent;
use super::WaitNode;
use core::{
    marker::PhantomData,
    sync::atomic::{fence, spin_loop_hint, AtomicUsize, Ordering},
};

const MUTEX_LOCK: usize = 1;
const QUEUE_LOCK: usize = 2;
const QUEUE_MASK: usize = !(QUEUE_LOCK | MUTEX_LOCK);

pub struct WordLock<E> {
    state: AtomicUsize,
    phantom: PhantomData<E>,
}

unsafe impl<E: ThreadEvent> lock_api::RawMutex for WordLock<E> {
    const INIT: Self = Self {
        state: AtomicUsize::new(0),
        phantom: PhantomData,
    };

    type GuardMarker = lock_api::GuardSend;

    fn try_lock(&self) -> bool {
        self.state
            .compare_exchange_weak(0, MUTEX_LOCK, Ordering::Acquire, Ordering::Relaxed)
            .is_ok()
    }

    fn lock(&self) {
        if !self.try_lock() {
            let node = WaitNode::<E>::default();
            self.lock_slow(&node);
        }
    }

    fn unlock(&self) {
        let state = self.state.fetch_sub(MUTEX_LOCK, Ordering::Release);
        if (state & QUEUE_MASK != 0) && (state & QUEUE_LOCK == 0) {
            self.unlock_slow();
        }
    }
}

impl<E: ThreadEvent> WordLock<E> {
    #[cold]
    fn lock_slow(&self, wait_node: &WaitNode<E>) {
        const MAX_SPIN_DOUBLING: usize = 4;

        let mut spin = 0;
        let mut state = self.state.load(Ordering::Relaxed);
        loop {
            // try to acquire the mutex if its unlocked
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

            // spin if theres no waiting nodes & havent spun too much
            let head = (state & QUEUE_MASK) as *const WaitNode<E>;
            if head.is_null() && spin < MAX_SPIN_DOUBLING {
                spin += 1;
                (0..(1 << spin)).for_each(|_| spin_loop_hint());
                state = self.state.load(Ordering::Relaxed);
                continue;
            }

            // try to enqueue our node to the wait queue
            let head = wait_node.enqueue(head);
            if let Err(s) = self.state.compare_exchange_weak(
                state,
                (head as usize) | (state & !QUEUE_MASK),
                Ordering::Release,
                Ordering::Relaxed,
            ) {
                state = s;
                continue;
            }

            // wait to be signaled by an unlocking thread
            if wait_node.wait() {
                return;
            } else {
                spin = 0;
                wait_node.reset();
                state = self.state.load(Ordering::Relaxed);
            }
        }
    }

    #[cold]
    fn unlock_slow(&self) {
        // acquire the queue lock in order to dequeue a node
        let mut state = self.state.load(Ordering::Relaxed);
        loop {
            if (state & QUEUE_MASK == 0) || (state & QUEUE_LOCK != 0) {
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
            // if the mutex is locked, let the under dequeue the node.
            if state & MUTEX_LOCK != 0 {
                match self.state.compare_exchange_weak(
                    state,
                    state & !QUEUE_LOCK,
                    Ordering::Relaxed,
                    Ordering::Relaxed,
                ) {
                    Ok(_) => return,
                    Err(s) => state = s,
                }
                fence(Ordering::Acquire);
                continue;
            }

            let head = unsafe { &*((state & QUEUE_MASK) as *const WaitNode<E>) };
            let (new_tail, tail) = head.dequeue();
            if new_tail.is_null() {
                loop {
                    // unlock the queue while zeroing the head since tail is last node
                    match self.state.compare_exchange_weak(
                        state,
                        state & MUTEX_LOCK,
                        Ordering::Release,
                        Ordering::Relaxed,
                    ) {
                        Ok(_) => break,
                        Err(s) => state = s,
                    }

                    // re process the queue if a new node comes in
                    if state & QUEUE_MASK != 0 {
                        fence(Ordering::Acquire);
                        continue 'outer;
                    }
                }
            } else {
                self.state.fetch_and(!QUEUE_LOCK, Ordering::Release);
            }

            // wake up the dequeued tail
            tail.notify(false);
            return;
        }
    }
}

unsafe impl<E: ThreadEvent> lock_api::RawMutexFair for WordLock<E> {
    fn unlock_fair(&self) {}

    fn bump(&self) {}
}
