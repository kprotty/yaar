use crate::event::{AutoResetEvent, AutoResetEventTimed, YieldContext};
use super::parker::{Parker, ParkResult, UnparkResult};
use core::{
    cell::UnsafeCell,
    fmt,
    mem::forget,
    ptr::NonNull,
    ops::{Deref, DerefMut},
    sync::atomic::{AtomicU8, Ordering},
};
use lock_api::{
    GuardSend,
    RawMutex,
    RawMutexFair,
    RawMutexTimed,
};

#[cfg(feature = "os")]
pub use if_os::*;

#[cfg(feature = "os")]
mod if_os {
    use super::*;
    use crate::event::OsAutoResetEvent;

    pub type Mutex<T> = GenericMutex<OsAutoResetEvent, T>;

    pub type MutexGuard<'a, T> = GenericMutexGuard<'a, OsAutoResetEvent, T>;

    pub type MappedMutexGuard<'a, T> = GenericMappedMutexGuard<'a, OsAutoResetEvent, T>;
}

const UNLOCKED: u8 = 0b00;
const LOCKED: u8 = 0b01;
const PARKED: u8 = 0b10;

const DEFAULT_TOKEN: usize = 0;
const RETRY_TOKEN: usize = 1;
const HANDOFF_TOKEN: usize = 2;

pub struct GenericRawMutex<E> {
    state: AtomicU8,
    parker: Parker<E>,
}

impl<E> GenericRawMutex<E> {
    pub const fn new() -> Self {
        Self {
            state: AtomicU8::new(UNLOCKED),
            parker: Parker::new(),
        }
    }
}

impl<E: AutoResetEvent> GenericRawMutex<E> {
    #[inline]
    fn acquire(&self, try_park: impl FnMut(&E) -> bool) -> bool {
        match self.state.compare_exchange_weak(
            UNLOCKED,
            LOCKED,
            Ordering::Acquire,
            Ordering::Relaxed,
        ) {
            Ok(_) => true,
            Err(_) => self.acquire_slow(try_park),
        }
    }

    #[inline]
    fn release(&self, be_fair: bool) {
        if let Err(_) = self.state.compare_exchange_weak(
            LOCKED,
            UNLOCKED,
            Ordering::Acquire,
            Ordering::Relaxed,
        ) {
            self.release_slow(be_fair);
        }
    }

    #[cold]
    fn acquire_slow(&self, mut try_park: impl FnMut(&E) -> bool)  -> bool {
        let mut spin: usize = 0;
        let mut state = self.state.load(Ordering::Relaxed);

        loop {
            if state & LOCKED == 0 {
                match self.state.compare_exchange_weak(
                    state,
                    state | LOCKED,
                    Ordering::Acquire,
                    Ordering::Relaxed,
                ) {
                    Ok(_) => return true,
                    Err(e) => state = e,
                }
                continue;
            }

            if E::yield_now(YieldContext {
                contended: state & PARKED != 0,
                iteration: spin,
                _sealed: (),
            }) {
                spin = spin.wrapping_add(1);
                state = self.state.load(Ordering::Relaxed);
                continue;
            }

            if state & PARKED == 0 {
                if let Err(e) = self.state.compare_exchange_weak(
                    state,
                    state | PARKED,
                    Ordering::Relaxed,
                    Ordering::Relaxed,
                ) {
                    state = e;
                    continue;
                }
            }

            let validate = || self.state.load(Ordering::Relaxed) == LOCKED | PARKED;
            let cancelled = |was_last_thread| {
                if was_last_thread {
                    self.state.fetch_and(!PARKED, Ordering::Relaxed);
                }
            };

            match unsafe {
                self.parker.park(
                    DEFAULT_TOKEN,
                    validate,
                    |event| try_park(event),
                    cancelled,
                )
            } {
                ParkResult::Invalid => {},
                ParkResult::Cancelled => return false,
                ParkResult::Unparked(HANDOFF_TOKEN) => return true,
                ParkResult::Unparked(RETRY_TOKEN) => {},
                _ => unreachable!(),
            }

            spin = 0;
            state = self.state.load(Ordering::Relaxed);
        }
    }

    #[cold]
    fn release_slow(&self, be_fair: bool) {
        let unpark = |_result, token| {
            debug_assert_eq!(token, DEFAULT_TOKEN);
            if be_fair { HANDOFF_TOKEN } else { RETRY_TOKEN }  
        };
        let callback = |result: UnparkResult| {
            if result.unparked != 0 && be_fair {
                if !result.has_more {
                    self.state.store(LOCKED, Ordering::Relaxed);
                }
            } else if result.has_more {
                self.state.store(PARKED, Ordering::Release);
            } else {
                self.state.store(UNLOCKED, Ordering::Release);
            }
        };
        unsafe {
            self.parker.unpark_one(unpark, callback);
        }
    }

    #[cold]
    fn bump_slow(&self) {
        self.release_slow(true);
        self.lock();
    }
}

unsafe impl<E: AutoResetEvent> RawMutex for GenericRawMutex<E> {
    const INIT: Self = Self::new();

    type GuardMarker = GuardSend;

    #[inline]
    fn try_lock(&self) -> bool {
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

    #[inline]
    fn lock(&self) {
        let acquired = self.acquire(|event| {
            event.wait();
            true
        });
        debug_assert!(acquired);
    }

    #[inline]
    fn unlock(&self) {
        self.release(false);
    }
}

unsafe impl<E: AutoResetEvent> RawMutexFair for GenericRawMutex<E> {
    #[inline]
    fn unlock_fair(&self) {
        self.release(true)
    }

    #[inline]
    fn bump(&self) {
        if self.state.load(Ordering::Relaxed) & PARKED != 0 {
            self.bump_slow();
        }
    }
}

unsafe impl<E: AutoResetEventTimed> RawMutexTimed for GenericRawMutex<E> where E::Instant: Copy {
    type Duration = E::Duration;
    type Instant = E::Instant;

    #[inline]
    fn try_lock_for(&self, mut timeout: Self::Duration) -> bool {
        self.acquire(|event| event.try_wait_for(&mut timeout))
    }

    #[inline]
    fn try_lock_until(&self, timeout: Self::Instant) -> bool {
        self.acquire(|event| event.try_wait_until(timeout))
    }
}

pub struct GenericMutex<E, T> {
    raw_mutex: GenericRawMutex<E>,
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
            raw_mutex: GenericRawMutex::new(),
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
        if self.raw_mutex.try_lock() {
            Some(GenericMutexGuard { mutex: self })
        } else {
            None
        }
    }

    #[inline]
    pub fn lock(&self) -> GenericMutexGuard<'_, E, T> {
        self.raw_mutex.lock();
        GenericMutexGuard { mutex: self }
    }

    #[inline]
    pub unsafe fn force_unlock(&self) {
        self.raw_mutex.unlock();
    }

    #[inline]
    pub unsafe fn force_unlock_fair(&self) {
        self.raw_mutex.unlock_fair();
    }
}

impl<E: AutoResetEventTimed, T> GenericMutex<E, T> where E::Instant: Copy {
    #[inline]
    pub fn try_lock_for(&self, timeout: E::Duration) -> Option<GenericMutexGuard<'_, E, T>> {
        if self.raw_mutex.try_lock_for(timeout) {
            Some(GenericMutexGuard { mutex: self })
        } else {
            None
        }
    }

    #[inline]
    pub fn try_lock_until(&self, timeout: E::Instant) -> Option<GenericMutexGuard<'_, E, T>> {
        if self.raw_mutex.try_lock_until(timeout) {
            Some(GenericMutexGuard { mutex: self })
        } else {
            None
        }
    }
}

impl<E: AutoResetEvent, T: fmt::Debug> fmt::Debug for GenericMutex<E, T> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self.try_lock() {
            Some(guard) => f.debug_struct("Mutex").field("data", &&*guard).finish(),
            None => f.debug_struct("Mutex").field("data", &"<locked>").finish(),
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
        self.mutex.raw_mutex.unlock();
    }
}

impl<'a, E: AutoResetEvent, T> GenericMutexGuard<'a, E, T> {
    #[inline]
    pub fn unlock_fair(this: Self) {
        this.mutex.raw_mutex.unlock_fair();
        forget(this)
    }

    #[inline]
    pub fn unlocked<U>(this: &mut Self, f: impl FnOnce() -> U) -> U {
        this.mutex.raw_mutex.unlock();
        let value = f();
        this.mutex.raw_mutex.lock();
        value
    }

    #[inline]
    pub fn unlocked_fair<U>(this: &mut Self, f: impl FnOnce() -> U) -> U {
        this.mutex.raw_mutex.unlock_fair();
        let value = f();
        this.mutex.raw_mutex.lock();
        value
    }
}

impl<'a, E: AutoResetEvent, T> GenericMutexGuard<'a, E, T> {
    #[inline]
    pub fn map<U>(
        this: Self,
        f: impl FnOnce(&mut T) -> &mut U,
    ) -> GenericMappedMutexGuard<'a, E, U> {
        unsafe {
            let raw_mutex = &this.mutex.raw_mutex;
            let value = f(&mut *this.mutex.value.get());
            let value = NonNull::new_unchecked(value);
            forget(this);
            GenericMappedMutexGuard { raw_mutex, value }
        }
    }

    #[inline]
    pub fn try_map<U>(
        this: Self,
        f: impl FnOnce(&mut T) -> Option<&mut U>,
    ) -> Result<GenericMappedMutexGuard<'a, E, U>, Self> {
        unsafe {
            let raw_mutex = &this.mutex.raw_mutex;
            if let Some(value) = f(&mut *this.mutex.value.get()) {
                let value = NonNull::new_unchecked(value);
                forget(this);
                Ok(GenericMappedMutexGuard { raw_mutex, value })
            } else {
                Err(this)
            }
        }
    }
}

#[must_use = "if unused the Mutex will immediately unlock"]
pub struct GenericMappedMutexGuard<'a, E: AutoResetEvent, T> {
    value: NonNull<T>,
    raw_mutex: &'a GenericRawMutex<E>,
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
        self.raw_mutex.unlock();
    }
}

impl<'a, E: AutoResetEvent, T> GenericMappedMutexGuard<'a, E, T> {
    #[inline]
    pub fn unlock_fair(this: Self) {
        this.raw_mutex.unlock_fair();
        forget(this)
    }

    #[inline]
    pub fn unlocked<U>(this: &mut Self, f: impl FnOnce() -> U) -> U {
        this.raw_mutex.unlock();
        let value = f();
        this.raw_mutex.lock();
        value
    }

    #[inline]
    pub fn unlocked_fair<U>(this: &mut Self, f: impl FnOnce() -> U) -> U {
        this.raw_mutex.unlock_fair();
        let value = f();
        this.raw_mutex.lock();
        value
    }
}

impl<'a, E: AutoResetEvent, T> GenericMappedMutexGuard<'a, E, T> {
    #[inline]
    pub fn map<U>(
        this: Self,
        f: impl FnOnce(&mut T) -> &mut U,
    ) -> GenericMappedMutexGuard<'a, E, U> {
        unsafe {
            let raw_mutex = this.raw_mutex;
            let value = f(&mut *this.value.as_ptr());
            let value = NonNull::new_unchecked(value);
            forget(this);
            GenericMappedMutexGuard { raw_mutex, value }
        }
    }

    #[inline]
    pub fn try_map<U>(
        this: Self,
        f: impl FnOnce(&mut T) -> Option<&mut U>,
    ) -> Result<GenericMappedMutexGuard<'a, E, U>, Self> {
        unsafe {
            let raw_mutex = this.raw_mutex;
            if let Some(value) = f(&mut *this.value.as_ptr()) {
                let value = NonNull::new_unchecked(value);
                forget(this);
                Ok(GenericMappedMutexGuard { raw_mutex, value })
            } else {
                Err(this)
            }
        }
    }
}
