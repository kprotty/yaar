use crate::{
    event::{AutoResetEvent, YieldContext},
    utils::UnwrapUnchecked,
};

use core::{
    cell::Cell,
    fmt,
    marker::PhantomData,
    mem::MaybeUninit,
    ptr::{drop_in_place, NonNull},
    sync::atomic::{fence, AtomicU8, AtomicUsize, Ordering},
};

const UNLOCKED: u8 = 0;
const LOCKED: u8 = 1;
const WAKING: usize = 256;
const MASK: usize = !(512 - 1);

#[repr(align(512))]
struct Waiter<E> {
    event: Cell<MaybeUninit<E>>,
    acquire_waking: Cell<MaybeUninit<bool>>,
    prev: Cell<MaybeUninit<Option<NonNull<Self>>>>,
    next: Cell<MaybeUninit<Option<NonNull<Self>>>>,
    tail: Cell<MaybeUninit<Option<NonNull<Self>>>>,
}

pub struct Lock<E> {
    state: AtomicUsize,
    _phantom: PhantomData<E>,
}

impl<E> fmt::Debug for Lock<E> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let state = self.state.load(Ordering::Relaxed);
        let state = match state & (LOCKED as usize) {
            0 => "<unlocked>",
            _ => "<locked>",
        };

        f.debug_struct("Mutex").field("state", &state).finish()
    }
}

impl<E> Default for Lock<E> {
    fn default() -> Self {
        Self::new()
    }
}

impl<E> Lock<E> {
    #[inline]
    pub const fn new() -> Self {
        Self {
            state: AtomicUsize::new(UNLOCKED as usize),
            _phantom: PhantomData,
        }
    }
}

unsafe impl<E: AutoResetEvent> lock_api::RawMutex for Lock<E> {
    const INIT: Self = Self::new();

    type GuardMarker = lock_api::GuardSend;

    #[inline]
    fn try_lock(&self) -> bool {
        let mut state = self.byte_state().load(Ordering::Relaxed);
        while state == UNLOCKED {
            match self.byte_state().compare_exchange_weak(
                UNLOCKED,
                LOCKED,
                Ordering::Acquire,
                Ordering::Relaxed,
            ) {
                Err(e) => state = e,
                Ok(_) => return true,
            }
        }
        false
    }

    #[inline]
    fn lock(&self) {
        if let Err(_) = self.byte_state().compare_exchange_weak(
            UNLOCKED,
            LOCKED,
            Ordering::Acquire,
            Ordering::Relaxed,
        ) {
            unsafe { self.lock_slow() };
        }
    }

    #[inline]
    fn unlock(&self) {
        self.byte_state().store(UNLOCKED, Ordering::Release);
        if self.state.load(Ordering::Relaxed) & MASK != 0 {
            unsafe { self.unlock_slow() };
        }
    }
}

impl<E: AutoResetEvent> Lock<E> {
    #[inline]
    fn byte_state(&self) -> &AtomicU8 {
        unsafe { &*(&self.state as *const AtomicUsize as *const AtomicU8) }
    }

    #[cold]
    unsafe fn lock_slow(&self) {
        // Lazyily allocate the waiter to improve fast path contended acquire.
        let mut spin: usize = 0;
        let mut event_initialized = false;
        let waiter = Waiter {
            event: Cell::new(MaybeUninit::uninit()),
            acquire_waking: Cell::new(MaybeUninit::uninit()),
            prev: Cell::new(MaybeUninit::uninit()),
            next: Cell::new(MaybeUninit::uninit()),
            tail: Cell::new(MaybeUninit::uninit()),
        };

        let mut state = self.state.load(Ordering::Relaxed);
        loop {
            // Try to acquire the lock if its unlocked.
            if state & (LOCKED as usize) == 0 {
                if let Ok(_) = self.byte_state().compare_exchange_weak(
                    UNLOCKED,
                    LOCKED,
                    Ordering::Acquire,
                    Ordering::Relaxed,
                ) {
                    // Make sure to drop the Event if it was created.
                    //
                    // Safety:
                    // Holding the lock guarantees our waiter isnt enqueued
                    // and hence accesible by another possible unlock() thread
                    // so it is ok to create a mutable reference to its fields.
                    if event_initialized {
                        let maybe_event = &mut *waiter.event.as_ptr();
                        drop_in_place(maybe_event.as_mut_ptr());
                    }
                    return;
                }
                state = self.state.load(Ordering::Relaxed);
                continue;
            }

            // Get the head of the waiter queue if any
            // and spin on the lock if the Event deems appropriate.
            let head = NonNull::new((state & MASK) as *mut Waiter<E>);
            if E::yield_now(YieldContext {
                contended: head.is_some(),
                iteration: spin,
                _sealed: (),
            }) {
                spin = spin.wrapping_add(1);
                state = self.state.load(Ordering::Relaxed);
                continue;
            }

            // Lazily initialize the Event if its not already
            if !event_initialized {
                event_initialized = true;
                waiter.prev.set(MaybeUninit::new(None));
                waiter.event.set(MaybeUninit::new(E::default()));
                waiter.acquire_waking.set(MaybeUninit::new(false));
            }

            // Prepare the waiter to be enqueued in the wait queue.
            // If its the first waiter, set it's tail to itself.
            // If not, then the tail will be resolved by a dequeing thread.
            let waiter_ptr = &waiter as *const _ as usize;
            waiter.next.set(MaybeUninit::new(head));
            waiter.tail.set(MaybeUninit::new(match head {
                Some(_) => None,
                None => NonNull::new(waiter_ptr as *mut _),
            }));

            // Try to enqueue the waiter as the new head of the wait queue.
            // Release barrier to make the waiter field writes visible to the deque thread.
            if let Err(e) = self.state.compare_exchange_weak(
                state,
                (state & !MASK) | waiter_ptr,
                Ordering::Release,
                Ordering::Relaxed,
            ) {
                state = e;
                continue;
            }

            // Park on our event until unparked by a dequeing thread.
            // Safety: guaranteed to be initialized from init_event.
            let maybe_event = &*waiter.event.as_ptr();
            let event_ref = &*maybe_event.as_ptr();
            event_ref.wait();

            // Reset everything and try to acquire the lock again.
            // Mark that we're awake so that another thread can be woken up in unlock().
            //
            // Safety: acquire_waking is guaranteed initialized by `event_initialized`
            // above.
            spin = 0;
            waiter.prev.set(MaybeUninit::new(None));
            if waiter.acquire_waking.get().assume_init() {
                waiter.acquire_waking.set(MaybeUninit::new(false));
                state = self.state.fetch_and(!WAKING, Ordering::Relaxed) & !WAKING;
            } else {
                state = self.state.load(Ordering::Relaxed);
            }
        }
    }

    #[cold]
    unsafe fn unlock_slow(&self) {
        // Try to acquire the WAKING flag on the mutex in order to deque->wake a waiting
        // thread. Bail if theres no threads to deque, theres already a thread
        // waiting, or the lock is being held since the lock holder can do the
        // wake on unlock().
        let mut state = self.state.load(Ordering::Relaxed);
        loop {
            if (state & MASK == 0) || (state & (WAKING | (LOCKED as usize)) != 0) {
                return;
            }
            match self.state.compare_exchange_weak(
                state,
                state | WAKING,
                Ordering::Acquire,
                Ordering::Relaxed,
            ) {
                Err(e) => state = e,
                Ok(_) => {
                    state |= WAKING;
                    break;
                }
            }
        }

        // Our thread is now the only one dequeing a node from the wait quuee.
        // On enter, and on re-loop, need an Acquire barrier in order to observe the
        // head waiter field writes from both the previous deque thread and the
        // enqueue thread.
        'deque: loop {
            // Safety: guaranteed non-null from the WAKING acquire loop above.
            let head = &*((state & MASK) as *const Waiter<E>);

            // Find the tail of the queue by following the next links from the head.
            // While traversing the links, store the prev link to complete the back-edge.
            // Finally, store the found tail onto the head to amortize the search later.
            let tail = {
                let mut current = head;
                loop {
                    if let Some(tail) = current.tail.get().assume_init() {
                        head.tail.set(MaybeUninit::new(Some(tail)));
                        break &*tail.as_ptr();
                    } else {
                        let next = current.next.get().assume_init();
                        let next = &*next.unwrap_unchecked().as_ptr();
                        let current_ptr = NonNull::from(current);
                        next.prev.set(MaybeUninit::new(Some(current_ptr)));
                        current = next;
                    }
                }
            };

            // The mutex is currently locked. Let the thread with the lock do the
            // dequeing at unlock by releasing our DEQUEUE privileges.
            //
            // Release barrier to make the queue link writes above visible to
            // the next unlock thread.
            if state & (LOCKED as usize) != 0 {
                match self.state.compare_exchange_weak(
                    state,
                    state & !WAKING,
                    Ordering::Release,
                    Ordering::Relaxed,
                ) {
                    Ok(_) => return,
                    Err(e) => state = e,
                }
                fence(Ordering::Acquire);
                continue 'deque;
            }

            // Get the new tail of the queue by traversing backwards
            // from the tail to the head following the prev links set above.
            let new_tail = tail.prev.get().assume_init();

            // The tail isnt the last waiter, update the tail of the queue.
            // Release barrier to make the queue link writes visible to next unlock thread.
            if let Some(new_tail) = new_tail {
                head.tail.set(MaybeUninit::new(Some(new_tail)));
                tail.acquire_waking.set(MaybeUninit::new(true));
                fence(Ordering::Release);

            // The tail was the last waiter in the queue.
            // Try to zero out the queue while also releasing the DEQUEUING
            // lock. If a new waiter comes in, we need to retry the
            // deque since it's next link would point to our dequed
            // tail.
            //
            // No Release barrier since after zeroing the queue,
            // no other thread can observe the waiters.
            } else {
                loop {
                    match self.state.compare_exchange_weak(
                        state,
                        state & (LOCKED as usize),
                        Ordering::Relaxed,
                        Ordering::Relaxed,
                    ) {
                        Ok(_) => break,
                        Err(e) => state = e,
                    }
                    if state & MASK != 0 {
                        fence(Ordering::Acquire);
                        continue 'deque;
                    }
                }
            }

            // Wake up the dequeud tail.
            let maybe_event = &*tail.event.as_ptr();
            let event_ref = &*maybe_event.as_ptr();
            event_ref.set();
            return;
        }
    }
}
