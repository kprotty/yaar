use super::ThreadParker;
use core::{
    cell::Cell,
    marker::PhantomData,
    mem::align_of,
    ptr::null,
    sync::atomic::{fence, spin_loop_hint, AtomicUsize, Ordering},
};

/// RawMutex implementation which uses the default OS implementation for thread parking
#[cfg(feature = "os")]
pub type Mutex<T> = RawMutex<T, super::OsThreadParker>;

/// MutexGuard for [`Mutex`].
#[cfg(feature = "os")]
pub type MutexGuard<'a, T> = RawMutexGuard<'a, T, super::OsThreadParker>;

/// Mutex abstract which utilizes [`WordLock`] from parking_lot
/// in order to implement a fair lock which is only a `usize` large.
/// This is platform agnostic and requires the user to provide their
/// own method for blocking the current thread via [`ThreadParker`].
///
/// [`WordLock`]: https://github.com/Amanieu/parking_lot/blob/master/core/src/word_lock.rs
pub type RawMutex<T, Parker> = lock_api::Mutex<WordLock<Parker>, T>;

/// MutexGuard for [`RawMutex`].
pub type RawMutexGuard<'a, T, Parker> = lock_api::MutexGuard<'a, WordLock<Parker>, T>;

#[doc(hidden)]
pub struct WordLock<Parker> {
    state: AtomicUsize,
    phantom: PhantomData<Parker>,
}

impl<Parker> WordLock<Parker> {
    pub const fn new() -> Self {
        Self {
            state: AtomicUsize::new(0),
            phantom: PhantomData,
        }
    }
}

const MUTEX_LOCK: usize = 1 << 0;
const QUEUE_LOCK: usize = 1 << 1;
const QUEUE_MASK: usize = !(MUTEX_LOCK | QUEUE_LOCK);

struct WaitQueueNode<Parker> {
    prev: Cell<*const Self>,
    next: Cell<*const Self>,
    tail: Cell<*const Self>,
    thread_parker: Parker,
}

unsafe impl<Parker: ThreadParker> lock_api::RawMutex for WordLock<Parker> {
    const INIT: Self = Self::new();

    type GuardMarker = lock_api::GuardSend;

    fn try_lock(&self) -> bool {
        // fast-path cas
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
        // Unlock the mutex immediately without looking at the queue.
        // fetch_sub(MUTEX) can be implemented more efficiently on
        // common platforms compared to fetch_and(!MUTEX_LOCK).
        let state = self.state.fetch_sub(MUTEX_LOCK, Ordering::Release);

        // If the queue isn't locked and there are nodes waiting,
        // go try and lock the queue in order to pop a node off and wake it up.
        if (state & QUEUE_LOCK == 0) && (state & QUEUE_MASK != 0) {
            unsafe { self.unlock_slow() };
        }
    }
}

impl<Parker: ThreadParker> WordLock<Parker> {
    #[cold]
    fn lock_slow(&self) {
        // Configurable spin count which incrementally increases
        // spinning on `spin_loop_hint()` up to 2^(N - 1)
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

        // Either spun too much or the queue is contended and has sleeping nodes
        // so we should prepare to block as well by creating our own node.
        assert!(align_of::<WaitQueueNode<Parker>>() > !QUEUE_MASK);
        let node = WaitQueueNode {
            prev: Cell::new(null()),
            next: Cell::new(null()),
            tail: Cell::new(null()),
            thread_parker: Parker::default(),
        };

        loop {
            // Anytime the mutex is unlocked, try to acquire it.
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

            // If the lock isnt contended and we haven't spun too much,
            // keep checking the MUTEX_LOCK state without invalidating its cache line.
            if (state & QUEUE_MASK == 0) && spin < SPIN_COUNT_DOUBLING {
                spin += 1;
                (0..(1 << spin)).for_each(|_| spin_loop_hint());
                state = self.state.load(Ordering::Relaxed);
                continue;
            }

            // The mutex is locked and we spun too much,
            // prepare our node to be put on the queue.
            // If the queue head is null, set the queue tail to ourselves.
            // If its not, then our tail will be computed by a thread in `unlock_slow()`.
            let head = (state & QUEUE_MASK) as *const WaitQueueNode<Parker>;
            node.next.set(head);
            if head.is_null() {
                node.tail.set(&node);
            } else {
                node.tail.set(null());
            }

            // Try to push ourselves onto the queue,
            // making available our node updates above with Release.
            if let Err(s) = self.state.compare_exchange_weak(
                state,
                (&node as *const _ as usize) | (state & !QUEUE_MASK),
                Ordering::Release,
                Ordering::Relaxed,
            ) {
                state = s;
                continue;
            }

            // We are now in the queue.
            // Wait to be notified by an unlocking thread.
            node.thread_parker.park();

            // Reset everything to prepare spinning on the lock again.
            spin = 0;
            node.prev.set(null());
            state = self.state.load(Ordering::Relaxed);
            node.thread_parker.reset();
        }
    }

    #[cold]
    unsafe fn unlock_slow(&self) {
        // In order to pop a node from the queue, we need to acquire the queue lock.
        let mut state = self.state.load(Ordering::Relaxed);
        loop {
            // stop trying if its already locked or if there are no nodes to dequeue.
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
            // The first node to appear in the queue sets the tail node to itself.
            // Later, once many nodes are pushed, an unprocess queue could look like so:
            //
            // [null:1, null:2, null:3, &4:4]
            //  ^head                   ^tail
            //
            // Given the head node of the queue, we need to find the tail node to dequeue.
            // So, starting from the head, until a node with the tail set appears, update
            // the `prev` pointers in order to chain correctly the doubly linked list.
            // This results in the head pointing to the tail:
            //
            // [&4:1, null:2, null:3, &4:4]
            //  ^head                 ^tail
            //
            // When a new node comes in as the head, the list only needs to be traversed
            // once since the old head points to the tail and can be found there instead
            // of traversing the entire list again.
            let head = &*((state & QUEUE_MASK) as *const WaitQueueNode<Parker>);
            let mut current = head;
            while current.tail.get().is_null() {
                let next = &*current.next.get();
                next.prev.set(current);
                current = next;
            }
            let tail = &*current.tail.get();
            head.tail.set(tail);

            // The `fence(Acquire)`s below are needed when observing a new
            // head as it will be dereferenced/read from so we should observe
            // any changes made to the head committed through a `Release` ordering.

            // If the mutex is locked, unlock the queue allowing
            // the mutex unlocker to take care of waking up the node.
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

            // Check if this is the last node in the queue.
            // If so, try to unlock the queue and set it to zero
            // by bitwise-anding only with the MUTEX_LOCK in case it becomes set.
            // Update the state using `Release` to make avaible to node updates above.
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

                    // If a new node is submitted to the queue while doing so,
                    // loop over again to update the nodes above.
                    if state & QUEUE_MASK != 0 {
                        fence(Ordering::Acquire);
                        continue 'outer;
                    }
                }

            // There are other nodes in the queue.
            // Update the tail to point tail.prev effectively popping the node.
            // Update the state using `Release` to make avaible to node updates.
            } else {
                head.tail.set(new_tail);
                self.state.fetch_and(!QUEUE_LOCK, Ordering::Release);
            }

            // Popped the tail from the queue. wake up its event.
            tail.thread_parker.unpark();
            return;
        }
    }
}
