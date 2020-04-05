use crate::{
    YieldContext,
    AutoResetEvent,
    AutoResetEventTimed,
    utils::UnwrapUnchecked,
};
use core::{
    cell::{Cell, UnsafeCell},
    fmt,
    marker::PhantomData,
    mem::{forget, MaybeUninit},
    ops::{Deref, DerefMut},
    ptr::{drop_in_place, NonNull},
    sync::atomic::{fence, Ordering, AtomicUsize},
};

#[cfg(feature = "os")]
pub use if_os::*;

#[cfg(feature = "os")]
mod if_os {
    use super::*;
    use crate::OsAutoResetEvent;

    pub type Mutex<T> = GenericMutex<OsAutoResetEvent, T>;

    pub type MutexGuard<'a, T> = GenericMutexGuard<'a, OsAutoResetEvent, T>;

    pub type MappedMutexGuard<'a, T> = GenericMappedMutexGuard<'a, OsAutoResetEvent, T>;
}

const UNLOCKED: usize = 0b00;
const LOCKED: usize = 0b01;
const DEQUEING: usize = 0b10;
const MASK: usize = !(UNLOCKED | LOCKED | DEQUEING);

#[repr(align(4))]
struct Waiter<E> {
    event: Cell<MaybeUninit<E>>,
    prev: Cell<MaybeUninit<Option<NonNull<Self>>>>,
    next: Cell<MaybeUninit<Option<NonNull<Self>>>>,
    tail: Cell<MaybeUninit<Option<NonNull<Self>>>>,
}

struct BlockingMutex<E> {
    state: AtomicUsize,
    _phantom: PhantomData<E>,
}

impl<E> BlockingMutex<E> {
    pub const fn new() -> Self {
        Self {
            state: AtomicUsize::new(UNLOCKED),
            _phantom: PhantomData,
        }
    }

    pub fn try_lock(&self) -> bool {
        let mut state = self.state.load(Ordering::Relaxed);
        while state & LOCKED == 0 {
            match self.state.compare_exchange_weak(
                state,
                state | LOCKED,
                Ordering::Acquire,
                Ordering::Relaxed,
            ) {
                Ok(_) => return true,
                Err(e) => state = e,
            }
        }
        false
    }
}

impl<E: AutoResetEvent> BlockingMutex<E> {
    pub unsafe fn lock(&self) {
        // Fast-path speculative lock acquire if uncontended
        if let Err(state) = self.state.compare_exchange_weak(
            UNLOCKED,
            LOCKED,
            Ordering::Acquire,
            Ordering::Relaxed,
        ) {
            self.lock_slow(state);
        }
    }

    #[cold]
    unsafe fn lock_slow(&self, mut state: usize) {
        let mut spin: usize = 0;
        let mut event_initialized = false;
        let waiter = Waiter {
            event: Cell::new(MaybeUninit::uninit()),
            prev: Cell::new(MaybeUninit::uninit()),
            next: Cell::new(MaybeUninit::uninit()),
            tail: Cell::new(MaybeUninit::uninit()),
        };

        loop {
            // Try to acquire the lock if its unlocked.
            if state & LOCKED == 0 {
                match self.state.compare_exchange_weak(
                    state,
                    state | LOCKED,
                    Ordering::Acquire,
                    Ordering::Relaxed,
                ) {
                    Err(e) => state = e,
                    Ok(_) => return {
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
                    },
                }
                continue;
            }

            // Get the head of the waiter queue if any
            // and spin on the lock if the Event deems appropriate.
            let head = NonNull::new((state & MASK) as *mut Waiter<E>);
            if E::yield_now(YieldContext {
                contended: head.is_some(),
                iteration: spin
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
            spin = 0;
            waiter.prev.set(MaybeUninit::new(None));
            state = self.state.load(Ordering::Relaxed);
        }
    }

    pub unsafe fn unlock(&self, be_fair: bool) {
        if be_fair {
            unimplemented!("TODO: fair mutex unlock");
        }

        // Unlock the mutex and try to deque->wake a waiting thread
        // if there are any and if another unlock() thread isn't already dequeing.
        let state = self.state.fetch_sub(LOCKED, Ordering::Release);
        if (state & MASK != 0) && (state & DEQUEING == 0) {
            self.unlock_slow(state - LOCKED);
        }
    }

    #[cold]
    unsafe fn unlock_slow(&self, mut state: usize) {
        // The lock was released, try to deque a waiter and unpark then to reacquire the lock.
        // Stop trying if there are no waiters or if another thread is already dequeing.
        loop {
            if (state & MASK == 0) || (state & DEQUEING != 0) {
                return;
            }
            match self.state.compare_exchange_weak(
                state,
                state | DEQUEING,
                Ordering::Acquire,
                Ordering::Relaxed,
            ) {
                Err(e) => state = e,
                Ok(_) => {
                    state |= DEQUEING;
                    break;
                },
            }
        }

        // Our thread is now the only one dequeing a node from the wait quuee.
        // On enter, and on re-loop, need an Acquire barrier in order to observe the head
        // waiter field writes from both the previous deque thread and the enqueue thread.
        'deque: loop {
            // Safety: guaranteed non-null from the DEQUEING acquire loop above.
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
                        let current_ptr = NonNull::new(current as *const _ as *mut _);
                        next.prev.set(MaybeUninit::new(current_ptr));
                        current = next;
                    }
                }
            };

            // The mutex is currently locked. Let the thread with the lock do the
            // dequeing at unlock by releasing our DEQUEUE privileges.
            //
            // Release barrier to make the queue link writes above visible to
            // the next unlock thread.
            if state & LOCKED != 0 {
                match self.state.compare_exchange_weak(
                    state,
                    state & !DEQUEING,
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
            
            // The tail isnt the last waiter.
            // Update the tail of the queue and release DEQUEING privileges.
            // Release barrier to make the queue link writes visible to next unlock thread.
            if let Some(new_tail) = new_tail {
                head.tail.set(MaybeUninit::new(Some(new_tail)));
                self.state.fetch_sub(DEQUEING, Ordering::Release);

            // The tail was the last waiter in the queue.
            // Try to zero out the queue while also releasing the DEQUEUING lock.
            // If a new waiter comes in, we need to retry the deque
            // since it's next link would point to our dequed tail.
            // 
            // No release barrier since after zeroing the queue,
            // no other thread can observe the waiters.
            } else {
                loop {
                    match self.state.compare_exchange_weak(
                        state,
                        state & LOCKED,
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

impl<E: AutoResetEventTimed> BlockingMutex<E> {
    #[inline]
    pub unsafe fn try_lock_for(&self, _timeout: E::Duration) -> bool {
        unimplemented!("TODO: timed mutex acquire")
    }

    #[inline]
    pub unsafe fn try_lock_until(&self, _timeout: E::Instant) -> bool {
        unimplemented!("TODO: tmied mutex acquire")
    }
}

pub struct GenericMutex<E, T> {
    blocking: BlockingMutex<E>,
    value: UnsafeCell<T>,
}

unsafe impl<E, T: Send> Send for GenericMutex<E, T> {}
unsafe impl<E, T: Send> Sync for GenericMutex<E, T> {}

impl<E, T: Default> Default for GenericMutex<E, T> {
    fn default() -> Self {
        Self::from(T::default())
    }
}

impl<E, T> From<T> for GenericMutex<E, T> {
    fn from(value: T) -> Self {
        Self::new(value)
    }
}

impl<E, T> GenericMutex<E, T> {
    pub const fn new(value: T) -> Self {
        Self {
            blocking: BlockingMutex::new(),
            value: UnsafeCell::new(value),
        }
    }

    pub fn into_inner(self) -> T {
        self.value.into_inner()
    }

    pub fn get_mut(&mut self) -> &mut T {
        unsafe { &mut *self.value.get() }
    }
}

impl<E: AutoResetEvent, T> GenericMutex<E, T> {
    #[inline]
    pub fn try_lock(&self) -> Option<GenericMutexGuard<'_, E, T>> {
        if self.blocking.try_lock() {
            Some(GenericMutexGuard { mutex: self })
        } else {
            None
        }
    }

    #[inline]
    pub fn lock(&self) -> GenericMutexGuard<'_, E, T> {
        unsafe { self.blocking.lock() };
        GenericMutexGuard { mutex: self }
    }

    #[inline]
    pub unsafe fn force_unlock(&self) {
        self.blocking.unlock(false)
    }

    #[inline]
    pub unsafe fn force_unlock_fair(&self) {
        self.blocking.unlock(true)
    }
}

impl<E: AutoResetEventTimed, T> GenericMutex<E, T> {
    #[inline]
    pub fn try_lock_for(
        &self,
        timeout: E::Duration,
    ) -> Option<GenericMutexGuard<'_, E, T>> {
        unsafe {
            if self.blocking.try_lock_for(timeout) {
                Some(GenericMutexGuard { mutex: self })
            } else {
                None
            }
        }
    }

    #[inline]
    pub fn try_lock_until(
        &self,
        timeout: E::Instant,
    ) -> Option<GenericMutexGuard<'_, E, T>> {
        unsafe {
            if self.blocking.try_lock_until(timeout) {
                Some(GenericMutexGuard { mutex: self })
            } else {
                None
            }
        }
    }
}

impl<E: AutoResetEvent, T: fmt::Debug> fmt::Debug for GenericMutex<E, T> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self.try_lock() {
            Some(guard) => f
                .debug_struct("Mutex")
                .field("data", &&*guard)
                .finish(),
            None => f
                .debug_struct("Mutex")
                .field("data", &"<locked>")
                .finish(),
        }
    }
}

#[must_use = "if unused the Mutex will immediately unlock"]
pub struct GenericMutexGuard<'a, E: AutoResetEvent, T> {
    mutex: &'a GenericMutex<E, T>,
}

unsafe impl<'a, E: AutoResetEvent, T: Send> Send for GenericMutexGuard<'a, E, T> {}
unsafe impl<'a, E: AutoResetEvent, T: Sync> Sync for GenericMutexGuard<'a, E, T> {}

impl<'a, E: AutoResetEvent, T> Deref for GenericMutexGuard<'a, E, T> {
    type Target = T;

    fn deref(&self) -> &T {
        unsafe { &*self.mutex.value.get() }
    }
}

impl<'a, E: AutoResetEvent, T> DerefMut for GenericMutexGuard<'a, E, T> {
    fn deref_mut(&mut self) -> &mut T {
        unsafe { &mut *self.mutex.value.get() }
    }
}

impl<'a, E: AutoResetEvent, T> Drop for GenericMutexGuard<'a, E, T> {
    fn drop(&mut self) {
        unsafe { self.mutex.blocking.unlock(false) }
    }
}

impl<'a, E: AutoResetEvent, T> GenericMutexGuard<'a, E, T> {
    #[inline]
    pub fn unlock_fair(this: Self) {
        unsafe { this.mutex.blocking.unlock(true) };
        forget(this)
    }

    #[inline]
    pub fn unlocked<U>(this: &mut Self, f: impl FnOnce() -> U) -> U {
        unsafe {
            this.mutex.blocking.unlock(false);
            let value = f();
            this.mutex.blocking.lock();
            value
        }
    }

    #[inline]
    pub fn unlocked_fair<U>(this: &mut Self, f: impl FnOnce() -> U) -> U {
        unsafe {
            this.mutex.blocking.unlock(true);
            let value = f();
            this.mutex.blocking.lock();
            value
        }
    }
}

impl<'a, E: AutoResetEvent, T> GenericMutexGuard<'a, E, T> {
    #[inline]
    pub fn map<U>(
        this: Self,
        f: impl FnOnce(&mut T) -> &mut U,
    ) -> GenericMappedMutexGuard<'a, E, U> {
        unsafe {
            let blocking = &this.mutex.blocking;
            let value = f(&mut *this.mutex.value.get());
            let value = NonNull::new_unchecked(value);
            forget(this);
            GenericMappedMutexGuard { blocking, value }
        }
    }

    #[inline]
    pub fn try_map<U>(
        this: Self,
        f: impl FnOnce(&mut T) -> Option<&mut U>,
    ) -> Result<GenericMappedMutexGuard<'a, E, U>, Self> {
        unsafe {
            let blocking = &this.mutex.blocking;
            if let Some(value) = f(&mut *this.mutex.value.get()) {
                let value = NonNull::new_unchecked(value);
                forget(this);
                Ok(GenericMappedMutexGuard { blocking, value })
            } else {
                Err(this)
            }
        }
    }
}

#[must_use = "if unused the Mutex will immediately unlock"]
pub struct GenericMappedMutexGuard<'a, E: AutoResetEvent, T> {
    value: NonNull<T>,
    blocking: &'a BlockingMutex<E>,
}

unsafe impl<'a, E: AutoResetEvent, T: Send> Send for GenericMappedMutexGuard<'a, E, T> {}
unsafe impl<'a, E: AutoResetEvent, T: Sync> Sync for GenericMappedMutexGuard<'a, E, T> {}

impl<'a, E: AutoResetEvent, T> Deref for GenericMappedMutexGuard<'a, E, T> {
    type Target = T;

    fn deref(&self) -> &T {
        unsafe { self.value.as_ref() }
    }
}

impl<'a, E: AutoResetEvent, T> DerefMut for GenericMappedMutexGuard<'a, E, T> {
    fn deref_mut(&mut self) -> &mut T {
        unsafe { self.value.as_mut() }
    }
}

impl<'a, E: AutoResetEvent, T> Drop for GenericMappedMutexGuard<'a, E, T> {
    fn drop(&mut self) {
        unsafe { self.blocking.unlock(false) }
    }
}

impl<'a, E: AutoResetEvent, T> GenericMappedMutexGuard<'a, E, T> {
    #[inline]
    pub fn unlock_fair(this: Self) {
        unsafe { this.blocking.unlock(true) };
        forget(this)
    }

    #[inline]
    pub fn unlocked<U>(this: &mut Self, f: impl FnOnce() -> U) -> U {
        unsafe {
            this.blocking.unlock(false);
            let value = f();
            this.blocking.lock();
            value
        }
    }

    #[inline]
    pub fn unlocked_fair<U>(this: &mut Self, f: impl FnOnce() -> U) -> U {
        unsafe {
            this.blocking.unlock(true);
            let value = f();
            this.blocking.lock();
            value
        }
    }
}

impl<'a, E: AutoResetEvent, T> GenericMappedMutexGuard<'a, E, T> {
    #[inline]
    pub fn map<U>(
        this: Self,
        f: impl FnOnce(&mut T) -> &mut U,
    ) -> GenericMappedMutexGuard<'a, E, U> {
        unsafe {
            let blocking = this.blocking;
            let value = f(&mut *this.value.as_ptr());
            let value = NonNull::new_unchecked(value);
            forget(this);
            GenericMappedMutexGuard { blocking, value }
        }
    }

    #[inline]
    pub fn try_map<U>(
        this: Self,
        f: impl FnOnce(&mut T) -> Option<&mut U>,
    ) -> Result<GenericMappedMutexGuard<'a, E, U>, Self> {
        unsafe {
            let blocking = this.blocking;
            if let Some(value) = f(&mut *this.value.as_ptr()) {
                let value = NonNull::new_unchecked(value);
                forget(this);
                Ok(GenericMappedMutexGuard { blocking, value })
            } else {
                Err(this)
            }
        }
    }
}
