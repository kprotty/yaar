use super::{ThreadEvent, WaitNode};
use core::{
    marker::PhantomData,
    ptr::null,
    sync::atomic::{fence, spin_loop_hint, AtomicUsize, Ordering},
};

/// A [`RawMutex`] backed by [`OsThreadEvent`].
///
/// [`OsThreadEvent`]: struct.OsThreadEvent.html
#[cfg(feature = "os")]
#[cfg_attr(feature = "nightly", doc(cfg(feature = "os")))]
pub type Mutex<T> = RawMutex<T, super::OsThreadEvent>;

/// A [`RawMutexGuard`] for [`Mutex`].
#[cfg(feature = "os")]
#[cfg_attr(feature = "nightly", doc(cfg(feature = "os")))]
pub type MutexGuard<'a, T> = RawMutexGuard<'a, T, super::OsThreadEvent>;

/// A [`lock_api::RawMutex`] backed by parking_lot's [`WordLock`].
///
/// [`WordLock`]: https://github.com/Amanieu/parking_lot/blob/master/core/src/word_lock.rs
pub type RawMutex<T, Event> = lock_api::Mutex<WordLock<Event>, T>;

/// A [`lock_api::MutexGuard`] for [`RawMutex`].
pub type RawMutexGuard<'a, T, Event> = lock_api::MutexGuard<'a, WordLock<Event>, T>;

#[doc(hidden)]
pub struct WordLock<Event: ThreadEvent> {
    state: AtomicUsize,
    phantom: PhantomData<Event>,
}

const MUTEX_LOCK: usize = 1 << 0;
const QUEUE_LOCK: usize = 1 << 1;
const QUEUE_MASK: usize = !(QUEUE_LOCK | MUTEX_LOCK);

unsafe impl<Event: ThreadEvent> lock_api::RawMutex for WordLock<Event> {
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
        // Only go into the slow path if doing a fast acquire fails.
        // Avoids allocating a WaitNode on the stack and better inlining.
        if !self.try_lock() {
            self.lock_slow();
        }
    }

    fn unlock(&self) {
        // Only go into the slow path if there is a node waiting
        // on the mutex and we could possibly lock the queue to wake it.
        // Uses fetch_sub(1) over fetch_and(!1) since the former can
        // be done wait-free instead of lock-free on common platforms.
        let state = self.state.fetch_sub(MUTEX_LOCK, Ordering::Release);
        if (state & QUEUE_LOCK == 0) && (state & QUEUE_MASK != 0) {
            self.unlock_slow();
        }
    }
}

impl<Event: ThreadEvent> WordLock<Event> {
    #[cold]
    fn lock_slow(&self) {
        let wait_node = WaitNode::<Event>::default();
        self.lock_using(&wait_node);
    }

    fn lock_using(&self, wait_node: &WaitNode<Event>) {
        const MAX_SPIN_DOUBLING: usize = 4;

        let mut spin = 0;
        let mut state = self.state.load(Ordering::Relaxed);
        loop {
            // try to acquire the mutex if its unlocked.
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

            // spin on the lock if the wait queue is empty
            let head = (state & QUEUE_MASK) as *const WaitNode<Event>;
            if head.is_null() && spin < MAX_SPIN_DOUBLING {
                spin += 1;
                (0..(1 << spin)).for_each(|_| spin_loop_hint());
                state = self.state.load(Ordering::Relaxed);
                continue;
            }

            // try to add our node to the wait queue
            wait_node.init(head);
            if let Err(s) = self.state.compare_exchange_weak(
                state,
                (&wait_node as *const _ as usize) | (state & !QUEUE_MASK),
                Ordering::Release,
                Ordering::Relaxed,
            ) {
                state = s;
                continue;
            }

            // Wait for our node to be notified, returning if it was directly
            // handed the mutex lock ownership or resetting to try acquire again
            // if it wasn't.
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
        // In order to dequeue a node for waking, the queue must be locked.
        // Use an Acquire barrier when acquiring the queue lock as explained below.
        let mut state = self.state.load(Ordering::Relaxed);
        loop {
            if (state & QUEUE_LOCK != 0) || (state & QUEUE_MASK == 0) {
                return;
            } else {
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
        }

        // We have the queue lock, try to dequeue the tail node to wake it up.
        // Anytime reiterating, make sure to use an Acquire barrier in order to
        // see any WaitNode memory writes to the head of the queue.
        'outer: loop {
            let (head, tail) = unsafe {
                let head = &*((state & QUEUE_MASK) as *const WaitNode<Event>);
                let tail = &*head.get_tail();
                (head, tail)
            };

            // If the mutex is locked, let the lock holder handle doing the node dequeue and
            // wake up by releasing the queue lock.
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

            // Release the queue lock & zero out the QUEUE_MASK if the tail was the last
            // node.
            let new_tail = unsafe { tail.get_prev() };
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

                    // Reprocess the queue since a new node was added.
                    if state & QUEUE_MASK != 0 {
                        fence(Ordering::Acquire);
                        continue 'outer;
                    }
                }

            // Not the last node, only release the queue lock.
            } else {
                head.set_tail(new_tail);
                self.state.fetch_and(!QUEUE_LOCK, Ordering::Release);
            }

            // wake up the waiting node without handoff so it has to
            // try and acquire the mutex normally again.
            tail.notify(false);
            return;
        }
    }
}

unsafe impl<Event: ThreadEvent> lock_api::RawMutexFair for WordLock<Event> {
    fn unlock_fair(&self) {
        let mut state = self.state.load(Ordering::Relaxed);
        loop {
            let head = (state & QUEUE_MASK) as *const WaitNode<Event>;

            // No node to dequeue, unlock the mutex normally without handoff.
            // Safe to use Relaxed ordering since no memory reads dependent
            // on the state have been done.
            if head.is_null() {
                match self.state.compare_exchange_weak(
                    state,
                    0,
                    Ordering::Relaxed,
                    Ordering::Relaxed,
                ) {
                    Ok(_) => return,
                    Err(s) => state = s,
                }
                continue;
            }

            // Read the head node & others with an acquire barrier
            // to ensure updates from other threads to the node are visible.
            fence(Ordering::Acquire);
            let (head, tail, new_tail) = unsafe {
                let head = &*head;
                let tail = &*head.get_tail();
                (head, tail, tail.get_prev())
            };

            // Dequeue the node without dropping the mutex lock.
            if new_tail.is_null() {
                if let Err(s) = self.state.compare_exchange_weak(
                    state,
                    MUTEX_LOCK,
                    Ordering::Release,
                    Ordering::Relaxed,
                ) {
                    state = s;
                    continue;
                }
            } else {
                head.set_tail(new_tail);
            }

            // wake up the dequeued node using handoff so it transfers
            // ownership of the mutex from us to the tail node.
            tail.notify(true);
            return;
        }
    }

    fn bump(&self) {
        // fast path: there is no node to dequeue.
        // Use an Acquire barrier since needs write visibility
        // to any updates done to the head and the other nodes.
        let state = self.state.load(Ordering::Acquire);
        let head = (state & QUEUE_MASK) as *const WaitNode<Event>;
        if head.is_null() {
            return;
        }

        let (head, tail, prev) = unsafe {
            let head = &*head;
            let tail = &*head.get_tail();
            (head, tail, tail.get_prev())
        };

        // Insert our wait_node to the tail of the queue
        // while dequeueing the existing tail.
        let wait_node = WaitNode::<Event>::default();
        wait_node.init(null());
        wait_node.set_prev(prev);
        head.set_tail(&wait_node);

        // Wake up the dequeued tail with a direct handoff.
        tail.notify(true);

        // Wait to re-acquire the lock again.
        if wait_node.wait() {
            return;
        } else {
            wait_node.reset();
            self.lock_using(&wait_node);
        }
    }
}
