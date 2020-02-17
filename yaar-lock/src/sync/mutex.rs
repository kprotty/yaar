use super::{SpinWait, WaitNode};
use crate::ThreadEvent;
use core::{
    marker::PhantomData,
    sync::atomic::{AtomicUsize, Ordering},
};

#[cfg(feature = "os")]
pub use self::if_os::*;
#[cfg(feature = "os")]
mod if_os {
    use super::*;
    use crate::OsThreadEvent;

    /// A [`CoreMutex`] backed by [`OsThreadEvent`].
    #[cfg_attr(feature = "nightly", doc(cfg(feature = "os")))]
    pub type Mutex<T> = RawMutex<T, OsThreadEvent>;

    /// A [`RawMutexGuard`] for [`Mutex`].
    #[cfg_attr(feature = "nightly", doc(cfg(feature = "os")))]
    pub type MutexGuard<'a, T> = RawMutexGuard<'a, T, OsThreadEvent>;
}

/// A mutual exclusion primitive useful for protecting shared data using
/// [`ThreadEvent`] for thread blocking.
pub type RawMutex<T, E> = lock_api::Mutex<CoreMutex<E>, T>;

/// An RAII implementation of a "scoped lock" of a [`RawMutex`].
/// When this structure is dropped (falls out of scope), the lock will be
/// unlocked.
///
/// The data protected by the mutex can be accessed through this guard via its
/// `Deref` and `DerefMut` implementations.
pub type RawMutexGuard<'a, T, E> = lock_api::MutexGuard<'a, CoreMutex<E>, T>;

const MUTEX_LOCK: usize = 1;
const QUEUE_LOCK: usize = 2;
const QUEUE_MASK: usize = !(QUEUE_LOCK | MUTEX_LOCK);

type QueueNode<E> = WaitNode<E, Notify>;
enum Notify {
    Retry,
    Acquire,
}

/// [`lock_api::RawMutex`] implementation of parking_lot's [`WordLock`].
///
/// [`WordLock`]: https://github.com/Amanieu/parking_lot/blob/master/core/src/word_lock.rs
pub struct CoreMutex<E: ThreadEvent> {
    state: AtomicUsize,
    phantom: PhantomData<E>,
}

unsafe impl<E: ThreadEvent> Send for CoreMutex<E> {}
unsafe impl<E: ThreadEvent> Sync for CoreMutex<E> {}

unsafe impl<E: ThreadEvent> lock_api::RawMutex for CoreMutex<E> {
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
            self.lock_slow();
        }
    }

    fn unlock(&self) {
        if self
            .state
            .compare_exchange(MUTEX_LOCK, 0, Ordering::Release, Ordering::Relaxed)
            .is_err()
        {
            self.unlock_slow();
        }
    }
}

impl<E: ThreadEvent> CoreMutex<E> {
    #[cold]
    fn lock_slow(&self) {
        let wait_node = QueueNode::new();
        let mut spin_wait = SpinWait::new();
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

            // spin if theres no waiting nodes & if we haven't spun too much.
            let head = (state & QUEUE_MASK) as *const QueueNode<E>;
            if head.is_null() && spin_wait.spin() {
                state = self.state.load(Ordering::Relaxed);
                continue;
            }

            // try to enqueue our node to the wait queue
            let head = wait_node.push(head, false);
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
            match wait_node.wait() {
                Notify::Acquire => return,
                Notify::Retry => {
                    wait_node.reset();
                    spin_wait.reset();
                    state = self.state.load(Ordering::Relaxed);
                }
            }
        }
    }

    #[cold]
    fn unlock_slow(&self) {
        // Unlock the mutex to let another possible thread acquire it while we dequeue.
        let mut state = self.state.fetch_sub(MUTEX_LOCK, Ordering::Release);
        loop {
            // give up if theres no nodes to dequeue or the queue is already locked.
            if (state & QUEUE_MASK == 0) || (state & QUEUE_LOCK != 0) {
                return;
            }

            // Try to lock the queue.
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

        // When dereferencing the head through the state variable, there needs to have
        // been an Acquire barrier. This is to observe the changes done to the
        // head node from the Release compare_exchange_weak in lock_slow().
        // Subsequentially, stores which unlock the queue after modifying nodes should
        // use Release to publish the changes to future unlock() threads which
        // Acquire the changes from the QUEUE_LOCK loop above.

        'outer: loop {
            // The head is safe to deref since its confirmed to be non-null with the queue
            // locking above.
            let head = unsafe { &*((state & QUEUE_MASK) as *const QueueNode<E>) };
            let tail = head.tail();

            // If the mutex is locked, let the locker thread dequeue the node.
            if state & MUTEX_LOCK != 0 {
                match self.state.compare_exchange_weak(
                    state,
                    state & !QUEUE_LOCK,
                    Ordering::AcqRel,
                    Ordering::Acquire,
                ) {
                    Ok(_) => return,
                    Err(s) => state = s,
                }
                continue;
            }

            let new_tail = head.pop(tail);
            if new_tail.is_null() {
                loop {
                    // unlock the queue while zeroing the head since tail is last node
                    match self.state.compare_exchange_weak(
                        state,
                        state & MUTEX_LOCK,
                        Ordering::AcqRel,
                        Ordering::Acquire,
                    ) {
                        Ok(_) => break,
                        Err(s) => state = s,
                    }

                    // re-process the queue if a new node comes in
                    if state & QUEUE_MASK != 0 {
                        continue 'outer;
                    }
                }
            } else {
                // unlock the queue without zero'ing the head since theres still more nodes.
                self.state.fetch_sub(QUEUE_LOCK, Ordering::Release);
            }

            // wake up the dequeued tail
            tail.notify(Notify::Retry);
            return;
        }
    }
}

#[cfg(test)]
#[test]
fn test_mutex() {
    use std::{
        sync::{atomic::AtomicBool, Arc, Barrier, Mutex},
        thread,
    };
    const NUM_THREADS: usize = 10;
    const NUM_ITERS: usize = 10_000;

    #[derive(Debug)]
    struct Context {
        /// Used to check if the critical section is really accessed by one
        /// thread
        is_exclusive: AtomicBool,
        /// Counter which is verified after running.
        /// Use u128 as most cpus cannot operate on it with one instruction.
        count: u128,
    }

    let start_barrier = Arc::new(Barrier::new(NUM_THREADS + 1));
    let context = Arc::new(Mutex::new(Context {
        is_exclusive: AtomicBool::new(false),
        count: 0,
    }));

    // Run NUM_THREAD thread which update the context count for NUM_ITERS each
    let workers = (0..NUM_THREADS)
        .map(|_| {
            let context = context.clone();
            let start_barrier = start_barrier.clone();
            thread::spawn(move || {
                start_barrier.wait();
                for _ in 0..NUM_ITERS {
                    let mut ctx = context.lock().unwrap();
                    assert_eq!(ctx.is_exclusive.swap(true, Ordering::SeqCst), false);
                    ctx.count += 1;
                    ctx.is_exclusive.store(false, Ordering::SeqCst);
                }
            })
        })
        .collect::<Vec<_>>();

    // Start the worker threads, wait for them to complete, and check if
    // incrementation is correct.
    start_barrier.wait();
    workers.into_iter().for_each(|t| t.join().unwrap());
    assert_eq!(
        context.lock().unwrap().count,
        (NUM_ITERS * NUM_THREADS) as u128
    );
}
