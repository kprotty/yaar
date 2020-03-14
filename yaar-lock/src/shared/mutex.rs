use super::Waiter;
use core::{
    marker::PhantomData,
    sync::atomic::{spin_loop_hint, Ordering, AtomicUsize},
};

pub struct Mutex<Signal> {
    state: AtomicUsize,
    phantom: PhantomData<Signal>,
}

const MUTEX_LOCK: usize = 1 << 0;
const QUEUE_LOCK: usize = 1 << 1;
const QUEUE_MASK: usize = !(MUTEX_LOCK | QUEUE_LOCK);

impl<Signal> Mutex<Signal> {
    pub const fn new() -> Self {
        Self {
            state: AtomicUsize::new(0),
            phantom: PhantomData,
        }
    }

    pub fn try_lock(&self) -> bool {
        self.state
            .compare_exchange(0, MUTEX_LOCK, Ordering::Acquire, Ordering::Relaxed)
            .is_ok()
    }

    pub fn lock_fast(&self) -> bool {
        self.state
            .compare_exchange_weak(0, MUTEX_LOCK, Ordering::Acquire, Ordering::Relaxed)
            .is_ok()
    }

    pub unsafe fn lock_slow<Signal>(
        &self,
        waiter: &Waiter<Signal>,
        init_signal: impl FnOnce() -> Signal,
    ) -> Result<(), &Signal> {
        let mut spin = 0;
        let mut state = self.state.load(Ordering::Relaxed);
        let init_signal = Cell::new(MaybeUninit::new(init_signal));

        loop {
            // Try to acquire the mutex if its unlocked
            if state & MUTEX_LOCK == 0 {
                match self.state.compare_exchange_weak(
                    state,
                    state | MUTEX_LOCK,
                    Ordering::Acquire,
                    Ordering::Relaxed,
                ) {
                    Ok(_) => return Ok(()),
                    Err(e) => state = e,
                }
                continue;
            }

            // Spin with backoff if the waiter queue is empty & we haven't spun too much.
            let head = (state & QUEUE_MASK) as *const Waiter<Signal>;
            if head.is_null() && spin < 6 {
                (0..(1 << spin)).for_each(|_| spin_loop_hint());
                spin += 1;
                state = self.state.load(Ordering::Relaxed);
                continue;
            }

            // Enqueue our waiter node into the wait queue.
            // Uses an `Ordering::Release` barrier to make the waiter writes visible to an unlock()'er.
            let new_head = Waiter::<Signal>::enqueue(head, waiter, &init_signal);
            match self.state.compare_exchange_weak(
                state,
                (new_head as usize) | (state & !QUEUE_MASK),
                Ordering::Release,
                Ordering::Relaxed,
            ) {
                Ok(_) => return Err(waiter.signal()),
                Err(e) => state = e,
            }
        }
    }

    /// Unlock the mutex to allow another thread to lock the mutex.
    /// Returns a reference to a Signal of the thread which the caller should resume.
    pub fn unlock<'a>(&self) -> Option<&'a Signal> {
        self.state
            .compare_exchange(MUTEX_LOCK, 0, Ordering::Release, Ordering::Relaxed)
            .map(|_| None)
            .unwrap_or_else(|_| unsafe { self.unlock_slow() })
    }

    #[cold]
    unsafe fn unlock_slow(&self) -> Option<&'a Signal> {
        // Unlock the mutex for other threads while we dequeue a waiter.
        let mut state = self.state.fetch_sub(MUTEX_LOCK, Ordering::Release) - MUTEX_LOCK;

        // Try to acquire the QUEUE_LOCK in order to dequeue a waiter.
        // Bail if the queue is already locked or if theres no waiters to dequeue.
        loop {
            if (state & QUEUE_LOCK != 0) || (state & QUEUE_MASK == 0) {
                return None;
            }
            match self.state.compare_exchange_weak(
                state,
                state | QUEUE_LOCK,
                Ordering::Acquire,
                Ordering::Relaxed,
            ) {
                Ok(_) => break,
                Err(e) => state = e,
            }
        }

        // After acquiring the QUEUE_LOCK, dequeue a waiter and release the QUEUE_LOCK.
        // The QUEUE_LOCK acquire above ensures that the head is never null.
        //
        // An `Ordering::Acquire` barrier is required before dereferencing the head
        // in order to see writes to the head from both a Waiter::enqueue() and Waiter::dequeue() thread.
        //
        // An `Ordering::Release` barrier is required after Waiter::find_tail() or Waiter::dequeue()
        // as both update the state of the queue and need to publish those writes to other threads.
        'outer: loop {
            let head = (state & QUEUE_MASK) as *const Waiter<Signal>;
            let tail = Waiter::<Signal>::find_tail(head);

            // If the mutex is currently locked, 
            // let the mutex locker do the dequeue by unlocking the queue for them.
            if state & MUTEX_LOCK != 0 {
                match self.state.compare_exchange_weak(
                    state,
                    state & ~QUEUE_LOCK,
                    Ordering::AcqRel,
                    Ordering::Acquire,
                ) {
                    Ok(_) => return None,
                    Err(e) => state = e,
                }
                continue;
            }

            // Dequeue the tail from the queue and
            // release the QUEUE_LOCK if the tail wasn't the last node. 
            let was_last_waiter = Waiter::<Signal>::dequeue(head, tail);
            if !was_last_waiter {
                self.state.fetch_sub(QUEUE_LOCK, Ordering::Release);
                return Some(tail.signal());
            }

            // The tail is the last waiter in the queue so the queue needs to be zeroed.
            // If another waiter gets enqueued while attempting to zero and release the QUEUE_LOCK
            // then we need to reloop and reprocess the head of the queue with this new waiter.
            loop {
                match self.state.compare_exchange_weak(
                    state,
                    state & MUTEX_LOCK,
                    Ordering::AcqRel,
                    Ordering::Acquire,
                ) {
                    Ok(_) => return Some(tail.signal()),
                    Err(e) => state = e,
                }
                if state & QUEUE_MASK != 0 {
                    Waiter::<Signal>::reset(head, tail);
                    continue 'outer;
                }
            }
        }
    }
}

