use crate::{AutoResetEvent, AutoResetEventTimed};
use super::GenericSignal;
use core::{
    cell::Cell,
    fmt,
    hint::unreachable_unchecked,
    mem::MaybeUninit,
    ptr::{drop_in_place, NonNull},
    sync::atomic::{fence, spin_loop_hint, AtomicUsize, Ordering},
};

#[cfg(feature = "os")]
pub use if_os::*;
#[cfg(feature = "os")]
mod if_os {
    use super::*;
    use crate::OsAutoResetEvent;

    /// A [`GenericMutex`] backed by [`OsAutoResetEvent`].
    pub type Mutex<T> = GenericMutex<T, OsAutoResetEvent>;

    /// A [`GenericMutexGuard`] for [`GenericMutex`]
    pub type MutexGuard<'a, T> = GenericMutexGuard<'a, T, OsAutoResetEvent>;
}

/// A [`RawMutex`] which uses a [`AutoResetEvent`] implementation for thread blocking.
pub type GenericMutex<T, E> = lock_api::Mutex<RawMutex<E>, T>;

/// A MutexGuard for some [`GenericMutex`] implementation.
pub type GenericMutexGuard<'a, T, E> = lock_api::MutexGuard<'a, GenericMutex<T, E>, T>;

const MUTEX_LOCK: usize = 1 << 0;
const QUEUE_LOCK: usize = 1 << 1;
const QUEUE_MASK: usize = !(QUEUE_LOCK | MUTEX_LOCK);

#[repr(align(4))]
struct Waiter<E> {
    event: Cell<MaybeUninit<E>>,
    prev: Cell<MaybeUninit<Option<NonNull<Self>>>>,
    next: Cell<MaybeUninit<Option<NonNull<Self>>>>,
    tail: Cell<MaybeUninit<Option<NonNull<Self>>>>,
}

/// An implementation of [`lock_api::RawMutex`] based on parking_lot's
/// [`WordLock`]
// [`WordLock`]: https://github.com/Amanieu/parking_lot/blob/master/core/src/word_lock.rs
#[derive(Default)]
pub struct RawMutex<E: AutoResetEvent> {
    state: AtomicUsize,
    signal: GenericSignal<E>,
}

impl<E: AutoResetEvent> fmt::Debug for RawMutex<E> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("RawMutex").finish()
    }
}

impl<E: AutoResetEvent + Default> Default for RawMutex<E> {
    fn default() -> Self {
        Self::INIT
    }
}

unsafe impl<E: AutoResetEvent + Default> lock_api::RawMutex for RawMutex<E> {
    const INIT: Self = Self {
        state: AtomicUsize::new(0),
        signal: GenericSignal::new(),
    };

    type GuardMarker = lock_api::GuardSend;

    #[inline]
    fn try_lock(&self) -> bool {
        self.state
            .compare_exchange(0, MUTEX_LOCK, Ordering::Acquire, Ordering::Relaxed)
            .is_ok()
    }

    #[inline]
    fn lock(&self) {
        if self
            .state
            .compare_exchange_weak(0, MUTEX_LOCK, Ordering::Acquire, Ordering::Relaxed)
            .is_err()
        {
            self.lock_slow(|event| {
                event.wait();
                false
            });
        }
    }

    #[inline]
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

unsafe impl<E: AutoResetEventTimed + Default> lock_api::RawMutexTimed for RawMutex<E> {
    type Duration = <E as AutoResetEventTimed>::Duration;
    type Instant = <E as AutoResetEventTimed>::Instant;

    #[inline]
    fn try_lock_for(&self, mut timeout: Self::Duration) {
        self.state
            .compare_exchange_weak(0, MUTEX_LOCK, Ordering::Acquire, Ordering::Relaxed)
            .map(|_| true)
            .unwrap_or_else(|_| {
                self.lock_slow(|event| {
                    event.try_wait_for(&mut timeout)
                })
            })
    }

    #[inline]
    fn try_lock_until(&self, mut timeout: Self::Instant) {
        self.state
            .compare_exchange_weak(0, MUTEX_LOCK, Ordering::Acquire, Ordering::Relaxed)
            .map(|_| true)
            .unwrap_or_else(|_| {
                self.lock_slow(|event| {
                    event.try_wait_until(&mut timeout)
                })
            })
    }
}

impl<E: AutoResetEvent + Default> RawMutex<E> {
    #[cold]
    fn lock_slow(&self, wait: impl Fn(&E) -> bool) -> bool {
        let mut event_initialized = false;
        let waiter = Waiter {
            event: Cell::new(MaybeUninit::uninit()),
            prev: Cell::new(MaybeUninit::uninit()),
            next: Cell::new(MaybeUninit::uninit()),
            tail: Cell::new(MaybeUninit::uninit()),
        };

        let mut spin = 0;
        let mut state = self.state.load(Ordering::Relaxed);

        loop {
            // Try to acquire the mutex if unlocked
            if state & MUTEX_LOCK == 0 {
                match self.state.compare_exchange_weak(
                    state,
                    state | MUTEX_LOCK,
                    Ordering::Acquire,
                    Ordering::Relaxed,
                ) {
                    Err(s) => state = s,
                    Ok(_) => {
                        if event_initialized {
                            // Safety:
                            // - Guaranteed not uninit() by event_initialized.
                            // - Not in the wait queue so can only be accessed by our thread.
                            unsafe {
                                let event_ptr = (&mut *waiter.event.as_ptr()).as_mut_ptr();
                                drop_in_place(event_ptr);
                            }
                        }
                        return true;
                    }
                }
                continue;
            }

            // Spin the mutex in a backoff fashion
            // if theres no one waiting in queue & if we haven't spun too much.
            let head = NonNull::new((state & QUEUE_MASK) as *mut Waiter<E>);
            if head.is_none() && !E::yield_now(spin) {
                spin = spin.wrapping_add(1);
                state = self.state.load(Ordering::Relaxed);
                continue;
            }

            // Lazy initialize the event as it could be potentially expensive to init on
            // fast path (e.g. some PTHREAD_MUTEX/COND_INITIALIERs)
            if !event_initialized {
                event_initialized = true;
                waiter.prev.set(MaybeUninit::new(None));
                waiter.event.set(MaybeUninit::new(E::default()));
            }

            // Prepare the waiter node to be enqueued.
            // If the first node in the queue, set tail to itself.
            waiter.next.set(MaybeUninit::new(head));
            if head.is_none() {
                let tail = NonNull::new(&waiter as *const _ as *mut _);
                waiter.tail.set(MaybeUninit::new(tail));
            } else {
                waiter.tail.set(MaybeUninit::new(None));
            }

            // Try to enqueue the waiter node in the wait queue.
            // Release ordering to make the node field updates above visible to dequeue
            // threads.
            if let Err(s) = self.state.compare_exchange_weak(
                state,
                (&waiter as *const _ as usize) | (state & !QUEUE_MASK),
                Ordering::Release,
                Ordering::Relaxed,
            ) {
                state = s;
                continue;
            }

            // Wait for the waiter's reset event to be notified.
            // If the wait times out, try to cancel the lock request.
            // 
            // Safety: Guaranteed initialization from higher code path on
            // event_initialized.
            unsafe {
                let event_ptr = (&*waiter.event.as_ptr()).as_ptr();
                if !wait(&*event_ptr) {
                    self.cancel(waiter, &*event_ptr);
                    drop_in_place(event_ptr as *mut _);
                    return false;
                }
            }

            // Retry the mutex acquire loop.
            spin = 0;
            waiter.prev.set(MaybeUninit::new(None));
            state = self.state.load(Ordering::Relaxed);
        }
    }

    #[cold]
    fn unlock_slow(&self) {
        // Unlock the mutex so other threads can acquire it while we dequeue & notify a
        // waiter.
        let mut state = self.state.fetch_sub(MUTEX_LOCK, Ordering::Release) - MUTEX_LOCK;

        // Try to acquire the queue lock in order to dequeue a waiter.
        // Bail if the queue is already locked or if theres nothing to dequeue.
        loop {
            if (state & QUEUE_LOCK != 0) || (state & QUEUE_MASK == 0) {
                return;
            }
            match self.state.compare_exchange_weak(
                state,
                state | QUEUE_LOCK,
                Ordering::Relaxed,
                Ordering::Relaxed,
            ) {
                Ok(_) => break,
                Err(e) => state = e,
            }
        }

        // QUEUE_LOCK is acquired, try to dequeue a waiter and notify its reset event.
        'outer: loop {
            unsafe {
                // Acquire fence synchronizes with both the Release CAS in lock_slow()
                // and the Release CAS's below in order for head's field updates to be visible.
                // Safety: head guaranteed not null from QUEUE_LOCK acquire above.
                fence(Ordering::Acquire);
                let head = &*((state & QUEUE_MASK) as *const Waiter<S>);

                // Find the tail of the queue starting from the head and following the next
                // links. This eventually terminates as the first waiter to
                // enqueue sets its tail to itself. While searching for the
                // tail, prev links are set in order to form a doubly-linked-list.
                // Once the tail is found, its stored on the head to amortize future tail
                // lookups.
                let mut current = head;
                let tail = loop {
                    match current.tail.get().assume_init() {
                        Some(tail) => {
                            head.tail.set(MaybeUninit::new(Some(tail)));
                            break &*tail.as_ptr();
                        }
                        None => {
                            let next = current.next.get().assume_init();
                            let next = next.unwrap_or_else(|| unreachable_unchecked());
                            let next = &*next.as_ptr();
                            let prev = NonNull::new(current as *const _ as *mut _);
                            next.prev.set(MaybeUninit::new(prev));
                            current = next;
                        }
                    }
                };

                // If the mutex is locked, let the locker thread handle dequeue'ing the waiter
                // by unlocking the queue. Release barrier in order to make the
                // tail + prev updates above visible to the locker thread.
                if state & MUTEX_LOCK != 0 {
                    match self.state.compare_exchange_weak(
                        state,
                        state & !QUEUE_LOCK,
                        Ordering::Release,
                        Ordering::Relaxed,
                    ) {
                        Ok(_) => return,
                        Err(e) => state = e,
                    }
                    continue;
                }

                match tail.prev.get().assume_init() {
                    // Dequeue the tail from the head of the queue when theres more waiters while
                    // unlocking the queue. Release ordering to make tail + prev
                    // updates above visible to next queue lock holder.
                    Some(new_tail) => {
                        head.tail.set(MaybeUninit::new(Some(new_tail)));
                        self.state.fetch_sub(QUEUE_LOCK, Ordering::Release);
                    }
                    // The tail is the head and is the last waiter in the queue so the queue needs
                    // to be zeroed. When zeroing + unlocking the queue, if a
                    // new waiter comes in, the queue needs to be reprocessed.
                    // This is to ensure that the new waiter doesn't store an incorrect next link to
                    // the dequeued tail. Relaxed ordering can be used here
                    // since once zeroed, no other thread can access or see writes to the tail.
                    None => loop {
                        match self.state.compare_exchange_weak(
                            state,
                            state & MUTEX_LOCK,
                            Ordering::Relaxed,
                            Ordering::Relaxed,
                        ) {
                            Ok(_) => break,
                            Err(e) => state = e,
                        }
                        if state & QUEUE_MASK != 0 {
                            continue 'outer;
                        }
                    },
                }

                // The tail waiter has officially been dequeued and the queue lock released.
                // Notify its reset event so that it can retry to acquire the mutex.
                let event = &*(&*tail.event.as_ptr()).as_ptr();
                event.set();
                return;
            }
        }
    }

    #[cold]
    fn cancel(waiter: &Waiter<E>, event: &E) {
        let mut state = self.state.load(Ordering::Relaxed);
        loop {
            if event.is_set() || (state & QUEUE_MASK == 0) {
                return;
            }

            if state & QUEUE_LOCK == 0 {
                match self.state.compare_exchange_weak(
                    state,
                    state | QUEUE_LOCK,
                    Ordering::Relaxed,
                    Ordering::Relaxed,
                ) {
                    Ok(_) => break,
                    Err(e) => state = e,
                }
                continue;
            }

            self.signal.wait();
            state = self.state.load(Ordering::Relaxed);
        }

        'outer: loop {

        }
    }
}

#[cfg(test)]
#[test]
fn test_mutex() {
    use std::{
        sync::{
            atomic::{AtomicBool, Ordering},
            Arc, Barrier,
        },
        thread,
    };
    const NUM_THREADS: usize = 3;
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
                    let mut ctx = context.lock();
                    assert_eq!(
                        ctx.is_exclusive.swap(true, Ordering::SeqCst),
                        false,
                        "Mutex lock() is not exclusive",
                    );
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
    assert_eq!(context.lock().count, (NUM_ITERS * NUM_THREADS) as u128);
}