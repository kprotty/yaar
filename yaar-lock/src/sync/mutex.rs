use super::GenericRawMutex;
/// Documentation copied and modified from lock_api.
use crate::event::{AutoResetEvent, AutoResetEventTimed};
use core::{
    cell::UnsafeCell,
    fmt,
    mem::forget,
    ops::{Deref, DerefMut},
    ptr::NonNull,
};
use lock_api::{RawMutex, RawMutexFair, RawMutexTimed};

#[cfg(feature = "os")]
pub use if_os::*;

#[cfg(feature = "os")]
mod if_os {
    use super::*;
    use crate::event::OsAutoResetEvent;

    /// A [`GenericMutex`] backed by [`OsAutoResetEvent`].
    #[cfg_attr(feature = "nightly", doc(cfg(feature = "os")))]
    pub type OsMutex<T> = GenericMutex<OsAutoResetEvent, T>;

    /// A [`GenericMutexMutex`] for [`Mutex`].
    #[cfg_attr(feature = "nightly", doc(cfg(feature = "os")))]
    pub type OsMutexGuard<'a, T> = GenericMutexGuard<'a, OsAutoResetEvent, T>;

    /// A [`GenericMappedMutexGuard`] for [`Mutex`].
    #[cfg_attr(feature = "nightly", doc(cfg(feature = "os")))]
    pub type OsMappedMutexGuard<'a, T> = GenericMappedMutexGuard<'a, OsAutoResetEvent, T>;
}

/// A mutual exclusion primitive useful for protecting shared data
///
/// This mutex will block threads waiting for the lock to become available. The
/// mutex can also be statically initialized or created via a `new`
/// constructor. Each mutex has a type parameter which represents the data that
/// it is protecting. The data can only be accessed through the RAII guards
/// returned from `lock` and `try_lock`, which guarantees that the data is only
/// ever accessed when the mutex is locked.
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
    /// Creates a new mutex in an unlocked state ready for use.
    pub const fn new(value: T) -> Self {
        Self {
            raw_mutex: GenericRawMutex::new(),
            value: UnsafeCell::new(value),
        }
    }

    /// Consumes this mutex, returning the underlying data.
    pub fn into_inner(self) -> T {
        self.value.into_inner()
    }

    /// Returns a mutable reference to the underlying data.
    ///
    /// Since this call borrows the `Mutex` mutably, no actual locking needs to
    /// take place---the mutable borrow statically guarantees no locks exist.
    pub fn get_mut(&mut self) -> &mut T {
        unsafe { &mut *self.value.get() }
    }
}

impl<E: AutoResetEvent, T> GenericMutex<E, T> {
    /// Attempts to acquire this lock.
    ///
    /// If the lock could not be acquired at this time, then `None` is returned.
    /// Otherwise, an RAII guard is returned. The lock will be unlocked when the
    /// guard is dropped.
    ///
    /// This function does not block.
    #[inline]
    pub fn try_lock(&self) -> Option<GenericMutexGuard<'_, E, T>> {
        if self.raw_mutex.try_lock() {
            Some(GenericMutexGuard { mutex: self })
        } else {
            None
        }
    }

    // Acquires a mutex, blocking the current thread until it is able to do so.
    /// This function will block the local thread until it is available to
    /// acquire the mutex. Upon returning, the thread is the only thread
    /// with the mutex held. An RAII guard is returned to allow scoped
    /// unlock of the lock. When the guard goes out of scope, the mutex will
    /// be unlocked.
    ///
    /// Attempts to lock a mutex in the thread which already holds the lock will
    /// result in a deadlock.
    #[inline]
    pub fn lock(&self) -> GenericMutexGuard<'_, E, T> {
        self.raw_mutex.lock();
        GenericMutexGuard { mutex: self }
    }

    /// Forcibly unlocks the mutex.
    ///
    /// This is useful when combined with `mem::forget` to hold a lock without
    /// the need to maintain a `GenericMutexGuard` object alive, for example
    /// when dealing with FFI.
    ///
    /// # Safety
    ///
    /// This method must only be called if the current thread logically owns a
    /// `GenericMutexGuard` but that guard has be discarded using `mem::forget`.
    /// Behavior is undefined if a mutex is unlocked when not locked.   
    #[inline]
    pub unsafe fn force_unlock(&self) {
        self.raw_mutex.unlock();
    }

    /// Forcibly unlocks the mutex using a fair unlock procotol.
    ///
    /// This is useful when combined with `mem::forget` to hold a lock without
    /// the need to maintain a `GenericMutexGuard` object alive, for example
    /// when dealing with FFI.
    ///
    /// # Safety
    ///
    /// This method must only be called if the current thread logically owns a
    /// `GenericMutexGuard` but that guard has be discarded using `mem::forget`.
    /// Behavior is undefined if a mutex is unlocked when not locked.
    #[inline]
    pub unsafe fn force_unlock_fair(&self) {
        self.raw_mutex.unlock_fair();
    }
}

impl<E: AutoResetEventTimed, T> GenericMutex<E, T> {
    /// Attempts to acquire this lock until a timeout is reached.
    ///
    /// If the lock could not be acquired before the timeout expired, then
    /// `None` is returned. Otherwise, an RAII guard is returned. The lock will
    /// be unlocked when the guard is dropped.
    #[inline]
    pub fn try_lock_for(&self, timeout: E::Duration) -> Option<GenericMutexGuard<'_, E, T>> {
        if self.raw_mutex.try_lock_for(timeout) {
            Some(GenericMutexGuard { mutex: self })
        } else {
            None
        }
    }

    /// Attempts to acquire this lock until a timeout is reached.
    ///
    /// If the lock could not be acquired before the timeout expired, then
    /// `None` is returned. Otherwise, an RAII guard is returned. The lock will
    /// be unlocked when the guard is dropped.
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

/// An RAII implementation of a "scoped lock" of a mutex. When this structure is
/// dropped (falls out of scope), the lock will be unlocked.
///
/// The data protected by the mutex can be accessed through this guard via its
/// `Deref` and `DerefMut` implementations.
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
    /// Unlocks the mutex using a fair unlock protocol.
    ///
    /// By default, mutexes are unfair and allow the current thread to re-lock
    /// the mutex before another has the chance to acquire the lock, even if
    /// that thread has been blocked on the mutex for a long time. This is the
    /// default because it allows much higher throughput as it avoids forcing a
    /// context switch on every mutex unlock. This can result in one thread
    /// acquiring a mutex many more times than other threads.
    ///
    /// However in some cases it can be beneficial to ensure fairness by forcing
    /// the lock to pass on to a waiting thread if there is one. This is done by
    /// using this method instead of dropping the `GenericMutexGuard` normally.
    #[inline]
    pub fn unlock_fair(this: Self) {
        this.mutex.raw_mutex.unlock_fair();
        forget(this)
    }

    // Temporarily unlocks the mutex to execute the given function.
    /// This is safe because `&mut` guarantees that there exist no other
    /// references to the data protected by the mutex.
    #[inline]
    pub fn unlocked<U>(this: &mut Self, f: impl FnOnce() -> U) -> U {
        this.mutex.raw_mutex.unlock();
        let value = f();
        this.mutex.raw_mutex.lock();
        value
    }

    /// Temporarily unlocks the mutex to execute the given function.
    ///
    /// The mutex is unlocked using a fair unlock protocol.
    ///
    /// This is safe because `&mut` guarantees that there exist no other
    /// references to the data protected by the mutex.
    #[inline]
    pub fn unlocked_fair<U>(this: &mut Self, f: impl FnOnce() -> U) -> U {
        this.mutex.raw_mutex.unlock_fair();
        let value = f();
        this.mutex.raw_mutex.lock();
        value
    }

    /// Makes a new `GenericMappedMutexGuard` for a component of the locked
    /// data.
    ///
    /// This operation cannot fail as the `GenericMutexGuard` passed
    /// in already locked the mutex.
    ///
    /// This is an associated function that needs to be
    /// used as `GenericMutexGuard::map(...)`. A method would interfere with
    /// methods of the same name on the contents of the locked data.
    #[inline]
    pub fn map<U>(
        this: Self,
        f: impl FnOnce(&mut T) -> &mut U,
    ) -> GenericMappedMutexGuard<'a, E, U> {
        let raw_mutex = &this.mutex.raw_mutex;
        let value = f(unsafe { &mut *this.mutex.value.get() });
        let value = NonNull::from(value);
        forget(this);
        GenericMappedMutexGuard { raw_mutex, value }
    }

    /// Attempts to make a new `GenericMappedMutexGuard` for a component of the
    /// locked data. The original guard is returned if the closure returns
    /// `None`.
    ///
    /// This operation cannot fail as the `GenericMutexGuard` passed
    /// in already locked the mutex.
    ///
    /// This is an associated function that needs to be
    /// used as `GenericMutexGuard::try_map(...)`. A method would interfere with
    /// methods of the same name on the contents of the locked data.
    #[inline]
    pub fn try_map<U>(
        this: Self,
        f: impl FnOnce(&mut T) -> Option<&mut U>,
    ) -> Result<GenericMappedMutexGuard<'a, E, U>, Self> {
        let raw_mutex = &this.mutex.raw_mutex;
        if let Some(value) = f(unsafe { &mut *this.mutex.value.get() }) {
            let value = NonNull::from(value);
            forget(this);
            Ok(GenericMappedMutexGuard { raw_mutex, value })
        } else {
            Err(this)
        }
    }
}

/// An RAII mutex guard returned by `GenericMutexGuard::map`, which can point to
/// a subfield of the protected data.
///
/// The main difference between `GenericMappedMutexGuard` and
/// `GenericMutexGuard` is that the former doesn't support temporarily unlocking
/// and re-locking, since that could introduce soundness issues if the locked
/// object is modified by another thread.
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
    /// Unlocks the mutex using a fair unlock protocol.
    ///
    /// By default, mutexes are unfair and allow the current thread to re-lock
    /// the mutex before another has the chance to acquire the lock, even if
    /// that thread has been blocked on the mutex for a long time. This is the
    /// default because it allows much higher throughput as it avoids forcing a
    /// context switch on every mutex unlock. This can result in one thread
    /// acquiring a mutex many more times than other threads.
    ///
    /// However in some cases it can be beneficial to ensure fairness by forcing
    /// the lock to pass on to a waiting thread if there is one. This is done by
    /// using this method instead of dropping the `GenericMutexGuard` normally.
    #[inline]
    pub fn unlock_fair(this: Self) {
        this.raw_mutex.unlock_fair();
        forget(this)
    }

    /// Makes a new `GenericMappedMutexGuard` for a component of the locked
    /// data.
    ///
    /// This operation cannot fail as the `GenericMappedMutexGuard` passed
    /// in already locked the mutex.
    ///
    /// This is an associated function that needs to be
    /// used as `GenericMappedMutexGuard::map(...)`. A method would interfere
    /// with methods of the same name on the contents of the locked data.
    #[inline]
    pub fn map<U>(
        this: Self,
        f: impl FnOnce(&mut T) -> &mut U,
    ) -> GenericMappedMutexGuard<'a, E, U> {
        let raw_mutex = this.raw_mutex;
        let value = f(unsafe { &mut *this.value.as_ptr() });
        let value = NonNull::from(value);
        forget(this);
        GenericMappedMutexGuard { raw_mutex, value }
    }

    /// Attempts to make a new `GenericMappedMutexGuard` for a component of the
    /// locked data. The original guard is returned if the closure returns
    /// `None`.
    ///
    /// This operation cannot fail as the `GenericMappedMutexGuard` passed
    /// in already locked the mutex.
    ///
    /// This is an associated function that needs to be
    /// used as `GenericMappedMutexGuard::try_map(...)`. A method would
    /// interfere with methods of the same name on the contents of the
    /// locked data.
    #[inline]
    pub fn try_map<U>(
        this: Self,
        f: impl FnOnce(&mut T) -> Option<&mut U>,
    ) -> Result<GenericMappedMutexGuard<'a, E, U>, Self> {
        let raw_mutex = this.raw_mutex;
        if let Some(value) = f(unsafe { &mut *this.value.as_ptr() }) {
            let value = NonNull::from(value);
            forget(this);
            Ok(GenericMappedMutexGuard { raw_mutex, value })
        } else {
            Err(this)
        }
    }
}
