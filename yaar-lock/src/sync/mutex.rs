use crate::{
    shared::{poll_sync, RawMutex},
    parker::{AutoResetEvent, AutoResetEventTimed},
};
use core::{
    cell::UnsafeCell,
    fmt,
    mem::forget,
    ops::{Deref, DerefMut},
    ptr::NonNull,
    task::Poll,
    hint::unreachable_unchecked,
};

#[cfg(feature = "os")]
pub use if_os::*;

#[cfg(feature = "os")]
mod if_os {
    use super::*;
    use crate::parker::OsAutoResetEvent;

    pub type Mutex<T> = GenericMutex<OsAutoResetEvent, T>;

    pub type MutexGuard<'a, T> = GenericMutexGuard<'a, OsAutoResetEvent, T>;

    pub type MappedMutexGuard<'a, T> = GenericMappedMutexGuard<'a, OsAutoResetEvent, T>;
}

struct BlockingMutex<E> {
    raw: RawMutex<E>,
}

impl<E> BlockingMutex<E> {
    pub const fn new() -> Self {
        Self {
            raw: RawMutex::new(),
        }
    }
}

impl<E: AutoResetEvent> BlockingMutex<E> {
    fn try_lock(&self) -> bool {
        self.raw.try_lock()
    }

    unsafe fn lock(&self) {
        let acquired = self.acquire(|event| {
            event.wait();
            true
        });
        debug_assert!(acquired);
    }

    unsafe fn unlock(&self, be_fair: bool) {
        self.raw.unlock(be_fair, |event| event.set());
    }

    unsafe fn acquire(&self, park: impl FnOnce(&E) -> bool) -> bool {
        self.raw.lock_fast() || self.acquire_slow(park)
    }

    #[cold]
    unsafe fn acquire_slow(&self, park: impl FnOnce(&E) -> bool) -> bool {
        let mut park = Some(park);
        poll_sync(self.raw.lock_slow(|event| Poll::Ready(
            park.take()
                .unwrap_or_else(|| unreachable_unchecked())
                (event)
        )))
    }
}

impl<E: AutoResetEventTimed> BlockingMutex<E> {
    #[inline]
    pub unsafe fn try_lock_for(&self, timeout: E::Duration) -> bool {
        self.acquire(|event| event.try_wait_for(timeout))
    }

    #[inline]
    pub unsafe fn try_lock_until(&self, timeout: E::Instant) -> bool {
        self.acquire(|event| event.try_wait_until(timeout))
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
