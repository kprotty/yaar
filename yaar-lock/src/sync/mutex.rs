use super::WaitNode;
use crate::ThreadEvent;
use core::{
    marker::PhantomData,
    sync::atomic::{fence, spin_loop_hint, AtomicUsize, Ordering},
};

#[cfg(feature = "os")]
#[cfg_attr(feature = "nightly", doc(cfg(feature = "os")))]
pub type Mutex<T> = RawMutex<T, crate::OsThreadEvent>;

#[cfg(feature = "os")]
#[cfg_attr(feature = "nightly", doc(cfg(feature = "os")))]
pub type MutexGuard<'a, T> = RawMutexGuard<'a, T, crate::OsThreadEvent>;

pub type RawMutex<T, E> = lock_api::Mutex<WordLock<E>, T>;
pub type RawMutexGuard<'a, T, E> = lock_api::MutexGuard<'a, WordLock<E>, T>;

const MUTEX_LOCK: usize = 1;
const QUEUE_LOCK: usize = 2;
const QUEUE_MASK: usize = !(QUEUE_LOCK | MUTEX_LOCK);

#[doc(hidden)]
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
            // give up if theres no nodes to dequeue or the queue is already locked.
            if (state & QUEUE_MASK == 0) || (state & QUEUE_LOCK != 0) {
                return;
            }

            // Try to lock the queue using an Acquire barrier on success
            // in order to have WaitNode write visibility. See below.
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

        // A Acquire barrier is required when looping back with a new state
        // since it will be dereferenced and read from as the head of the queue
        // and updates to its fields need to be visible from the Release store in `lock_slow()`.
        'outer: loop {
            // If the mutex is locked, let the under dequeue the node.
            // Safe to use Relaxed on success since not making any memory writes visible.
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

            // The head is safe to deref since its confirmed when queue locking above.
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

                    // re-process the queue if a new node comes in
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
    fn unlock_fair(&self) {
        let mut state = self.state.load(Ordering::Relaxed);
        loop {
            // there aren't any nodes to dequeue or the queue is locked.
            // try to unlock the mutex normally without dequeued a node.
            if (state & QUEUE_MASK == 0) || (state & QUEUE_LOCK != 0) {
                match self.state.compare_exchange_weak(
                    state,
                    state & QUEUE_LOCK,
                    Ordering::Relaxed,
                    Ordering::Relaxed,
                ) {
                    Ok(_) => return,
                    Err(s) => state = s,
                }
            // The queue is unlocked and theres a node to remove.
            // try to lock the queue in order to dequeue & wake the node.
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
        
        'outer: loop {
            // The head is safe to deref since its confirmed when queue locking above.
            let head = unsafe { &*((state & QUEUE_MASK) as *const WaitNode<E>) };
            let (new_tail, tail) = head.dequeue();

            // update the state to dequeue with a Release ordering which 
            // publishes the writes done by `.dequeue()` to other threads.
            if new_tail.is_null() {
                loop {
                    // unlock the queue while zeroing the head since tail is last node.
                    match self.state.compare_exchange_weak(
                        state,
                        MUTEX_LOCK,
                        Ordering::Release,
                        Ordering::Relaxed,
                    ) {
                        Ok(_) => break,
                        Err(s) => state = s,
                    }

                    // Re-process the queue if a new node comes in.
                    // See `unlock_slow()` for the reasoning on the Acquire fence.
                    if state & QUEUE_MASK != 0 {
                        fence(Ordering::Acquire);
                        continue 'outer;
                    }
                }
            } else {
                self.state.fetch_and(!QUEUE_LOCK, Ordering::Release);
            }

            // wake up the node with the mutex still locked (direct handoff)
            tail.notify(true);
            return;
        }
    }

    // TODO: bump()
}

#[cfg(test)]
#[test]
fn test_mutex() {
    use std::{thread, sync::{Arc, Mutex, Barrier, atomic::AtomicBool}};
    const NUM_THREADS: usize = 10;
    const NUM_ITERS: usize = 100_000;

    #[derive(Debug)]
    struct Context {
        is_exclusive: AtomicBool,
        count: u128,
    }

    let start_barrier = Arc::new(Barrier::new(NUM_THREADS + 1));
    let context = Arc::new(Mutex::new(Context {
        is_exclusive: AtomicBool::new(false),
        count: 0,
    }));

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
    start_barrier.wait();
    workers.into_iter().for_each(|t| t.join().unwrap());
    assert_eq!(context.lock().unwrap().count, (NUM_ITERS * NUM_THREADS) as u128);
}