// Contains lots of code lifted from futures-intrusive, parking_lot, and lock_api.
// Major credit to those crates for influencing this one.

use crate::shared::mutex::{WaitNode, WordLock, WAIT_NODE_INIT};
use core::{
    cell::{Cell, UnsafeCell},
    fmt,
    future::Future,
    mem::{self, MaybeUninit},
    ops::{Deref, DerefMut},
    pin::Pin,
    task::{Context, Poll, Waker},
};

/// A mutual exclusion primitive useful for protecting shared data
///
/// This mutex will block futures waiting for the lock to become available. The
/// mutex can also be statically initialized or created via a `new`
/// constructor. Each mutex has a type parameter which represents the data that
/// it is protecting. The data can only be accessed through the RAII guards
/// returned from `lock` and `try_lock`, which guarantees that the data is only
/// ever accessed when the mutex is locked.
pub struct Mutex<T> {
    value: UnsafeCell<T>,
    lock: WordLock,
}

unsafe impl<T: Send> Send for Mutex<T> {}
unsafe impl<T: Send> Sync for Mutex<T> {}

impl<T> From<T> for Mutex<T> {
    fn from(value: T) -> Self {
        Self::new(value)
    }
}

impl<T: Default> Default for Mutex<T> {
    fn default() -> Self {
        Self::new(T::default())
    }
}

impl<T: fmt::Debug> fmt::Debug for Mutex<T> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self.try_lock() {
            Some(guard) => f.debug_struct("Mutex").field("data", &&*guard).finish(),
            None => {
                struct LockedPlaceholder;
                impl fmt::Debug for LockedPlaceholder {
                    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                        f.write_str("<locked>")
                    }
                }
                f.debug_struct("Mutex")
                    .field("data", &LockedPlaceholder)
                    .finish()
            }
        }
    }
}

impl<T> Mutex<T> {
    /// Creates a new futures-aware mutex.
    pub const fn new(value: T) -> Self {
        Self {
            value: UnsafeCell::new(value),
            lock: WordLock::new(),
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

    /// Tries to acquire the mutex
    ///
    /// If acquiring the mutex is successful, a [`MutexGuard`]
    /// will be returned, which allows to access the contained data.
    ///
    /// Otherwise `None` will be returned.
    #[inline]
    pub fn try_lock(&self) -> Option<MutexGuard<'_, T>> {
        if self.lock.try_lock() {
            Some(MutexGuard { mutex: self })
        } else {
            None
        }
    }

    /// Acquire the mutex asynchronously.
    ///
    /// This method returns a future that will resolve once the mutex has been
    /// successfully acquired.
    #[inline]
    pub fn lock(&self) -> FutureLock<'_, T> {
        FutureLock {
            mutex: self,
            wait_node: WaitNode::default(),
        }
    }
}

/// Future used to acquire a guard to a Mutex.
pub struct FutureLock<'a, T> {
    mutex: &'a Mutex<T>,
    wait_node: WaitNode<Waker>,
}

unsafe impl<'a, T> Send for FutureLock<'a, T> {}

impl<'a, T> Future for FutureLock<'a, T> {
    type Output = MutexGuard<'a, T>;

    fn poll(self: Pin<&mut Self>, ctx: &mut Context<'_>) -> Poll<Self::Output> {
        if self
            .mutex
            .lock
            .lock(0, &self.wait_node, || ctx.waker().clone())
        {
            Poll::Ready(MutexGuard { mutex: self.mutex })
        } else {
            self.wait_node
                .flags
                .set(self.wait_node.flags.get() & !WAIT_NODE_INIT);
            Poll::Pending
        }
    }
}

/// An RAII implementation of a "scoped lock" of a mutex. When this structure is
/// dropped (falls out of scope), the lock will be unlocked.
///
/// The data protected by the mutex can be accessed through this guard via its
/// `Deref` and `DerefMut` implementations.
pub struct MutexGuard<'a, T> {
    mutex: &'a Mutex<T>,
}

#[inline]
fn wake_node(node: &WaitNode<Waker>) {
    unsafe {
        node.waker
            .replace(MaybeUninit::uninit())
            .assume_init()
            .wake()
    }
}

unsafe impl<'a, T: Send> Send for MutexGuard<'a, T> {}
unsafe impl<'a, T: Sync> Sync for MutexGuard<'a, T> {}

impl<'a, T> Drop for MutexGuard<'a, T> {
    fn drop(&mut self) {
        if let Some(node) = self.mutex.lock.unlock_unfair() {
            wake_node(node);
        }
    }
}

impl<'a, T> DerefMut for MutexGuard<'a, T> {
    fn deref_mut(&mut self) -> &mut T {
        unsafe { &mut *self.mutex.value.get() }
    }
}

impl<'a, T> Deref for MutexGuard<'a, T> {
    type Target = T;

    fn deref(&self) -> &Self::Target {
        unsafe { &*self.mutex.value.get() }
    }
}

impl<'a, T: fmt::Debug> fmt::Debug for MutexGuard<'a, T> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        fmt::Debug::fmt(&**self, f)
    }
}

impl<'a, T: fmt::Display> fmt::Display for MutexGuard<'a, T> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        (&**self).fmt(f)
    }
}

impl<'a, T> MutexGuard<'a, T> {
    /// Makes a new `MappedMutexGuard` for a component of the locked data.
    ///
    /// This operation cannot fail as the `MutexGuard` passed
    /// in already locked the mutex.
    ///
    /// This is an associated function that needs to be
    /// used as `MutexGuard::map(...)`. A method would interfere with methods of
    /// the same name on the contents of the locked data.
    pub fn map<U>(this: Self, f: impl FnOnce(&mut T) -> &mut U) -> MappedMutexGuard<'a, U> {
        let lock = &this.mutex.lock;
        let value = f(unsafe { &mut *this.mutex.value.get() });
        mem::forget(this);
        MappedMutexGuard { lock, value }
    }

    /// Attempts to make a new `MappedMutexGuard` for a component of the
    /// locked data. The original guard is returned if the closure returns `None`.
    ///
    /// This operation cannot fail as the `MutexGuard` passed
    /// in already locked the mutex.
    ///
    /// This is an associated function that needs to be
    /// used as `MutexGuard::try_map(...)`. A method would interfere with methods of
    /// the same name on the contents of the locked data.
    pub fn try_map<U>(
        this: Self,
        f: impl FnOnce(&mut T) -> Option<&mut U>,
    ) -> Result<MappedMutexGuard<'a, U>, Self> {
        match f(unsafe { &mut *this.mutex.value.get() }) {
            None => Err(this),
            Some(value) => {
                let lock = &this.mutex.lock;
                mem::forget(this);
                Ok(MappedMutexGuard { lock, value })
            }
        }
    }

    /// Unlocks the mutex using a fair unlock protocol.
    ///
    /// By default, mutexes are unfair and allow the current future to re-lock
    /// the mutex before another has the chance to acquire the lock, even if
    /// that future has been blocked on the mutex for a long time. This can
    /// result in one future acquiring a mutex many more times than other futures.
    ///
    /// However in some cases it can be beneficial to ensure fairness by forcing
    /// the lock to pass on to a waiting thread if there is one. This is done by
    /// using this method instead of dropping the `MutexGuard` normally.
    #[inline]
    pub fn unlock_fair(self) {
        if let Some(node) = self.mutex.lock.unlock_fair() {
            wake_node(node);
        }
        mem::forget(self);
    }

    /// Temporarily yields the mutex to a waiting thread if there is one.
    ///
    /// This method is functionally equivalent to calling `unlock_fair` followed
    /// by `lock`, however it can be much more efficient in the case where there
    /// are no waiting threads.
    #[inline]
    pub fn bump(&'a mut self) -> FutureUnlock<'a, (), Self> {
        // TODO: replace with self.lock.bump()
        self.unlocked_fair(|| {})
    }

    /// Temporarily unlocks the mutex to execute the given function.
    ///
    /// This is safe because `&mut` guarantees that there exist no other
    /// references to the data protected by the mutex.
    pub fn unlocked<R: 'a>(&'a mut self, f: impl FnOnce() -> R + 'a) -> FutureUnlock<'a, R, Self> {
        let lock = &self.mutex.lock;
        if let Some(node) = lock.unlock_unfair() {
            wake_node(node);
        }
        FutureUnlock {
            lock: lock,
            _guard: self,
            wait_node: WaitNode::default(),
            output: Cell::new(MaybeUninit::new(f())),
        }
    }

    /// Temporarily unlocks the mutex to execute the given function.
    ///
    /// The mutex is unlocked using a fair unlock protocol.
    ///
    /// This is safe because `&mut` guarantees that there exist no other
    /// references to the data protected by the mutex.
    pub fn unlocked_fair<R: 'a>(
        &'a mut self,
        f: impl FnOnce() -> R + 'a,
    ) -> FutureUnlock<'a, R, Self> {
        let lock = &self.mutex.lock;
        if let Some(node) = lock.unlock_unfair() {
            wake_node(node);
        }
        FutureUnlock {
            lock,
            _guard: self,
            wait_node: WaitNode::default(),
            output: Cell::new(MaybeUninit::new(f())),
        }
    }
}

/// Future used to re-acquire the mutex
/// from an `unlocked*` function in `MutexGuard*`
pub struct FutureUnlock<'a, T, M> {
    lock: &'a WordLock,
    _guard: &'a mut M,
    wait_node: WaitNode<Waker>,
    output: Cell<MaybeUninit<T>>,
}

unsafe impl<'a, T, M> Send for FutureUnlock<'a, T, M> {}

impl<'a, T, M> Future for FutureUnlock<'a, T, M> {
    type Output = T;

    // After acquiring the mutex, return the output by consuming it.
    // Subsequent calls to poll after returning `Poll::Ready()` is UB
    // as it calls `MaybeUninit::assume_init()` on a `MaybeUninit::uninit()`.
    fn poll(self: Pin<&mut Self>, ctx: &mut Context<'_>) -> Poll<Self::Output> {
        if self.lock.lock(0, &self.wait_node, || ctx.waker().clone()) {
            Poll::Ready(unsafe {
                mem::replace(&mut *self.output.as_ptr(), MaybeUninit::uninit()).assume_init()
            })
        } else {
            self.wait_node
                .flags
                .set(self.wait_node.flags.get() & !WAIT_NODE_INIT);
            Poll::Pending
        }
    }
}

/// An RAII mutex guard returned by `MutexGuard::map`, which can point to a
/// subfield of the protected data.
///
/// The main difference between `MappedMutexGuard` and `MutexGuard` is that the
/// former doesn't support temporarily unlocking and re-locking, since that
/// could introduce soundness issues if the locked object is modified by another
/// thread.
pub struct MappedMutexGuard<'a, T> {
    lock: &'a WordLock,
    value: *mut T,
}

unsafe impl<'a, T: Send> Send for MappedMutexGuard<'a, T> {}
unsafe impl<'a, T: Sync> Sync for MappedMutexGuard<'a, T> {}

impl<'a, T> Drop for MappedMutexGuard<'a, T> {
    fn drop(&mut self) {
        if let Some(node) = self.lock.unlock_unfair() {
            wake_node(node);
        }
    }
}

impl<'a, T> DerefMut for MappedMutexGuard<'a, T> {
    fn deref_mut(&mut self) -> &mut T {
        unsafe { &mut *self.value }
    }
}

impl<'a, T> Deref for MappedMutexGuard<'a, T> {
    type Target = T;

    fn deref(&self) -> &Self::Target {
        unsafe { &*self.value }
    }
}

impl<'a, T: fmt::Debug> fmt::Debug for MappedMutexGuard<'a, T> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        fmt::Debug::fmt(&**self, f)
    }
}

impl<'a, T: fmt::Display> fmt::Display for MappedMutexGuard<'a, T> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        (&**self).fmt(f)
    }
}

impl<'a, T: 'a> MappedMutexGuard<'a, T> {
    /// Makes a new `MappedMutexGuard` for a component of the locked data.
    ///
    /// This operation cannot fail as the `MappedMutexGuard` passed
    /// in already locked the mutex.
    ///
    /// This is an associated function that needs to be
    /// used as `MappedMutexGuard::map(...)`. A method would interfere with methods of
    /// the same name on the contents of the locked data.
    pub fn map<U>(this: Self, f: impl FnOnce(&mut T) -> &mut U) -> MappedMutexGuard<'a, U> {
        let lock = this.lock;
        let value = f(unsafe { &mut *this.value });
        mem::forget(this);
        MappedMutexGuard { lock, value }
    }

    /// Attempts to make a new `MappedMutexGuard` for a component of the
    /// locked data. The original guard is returned if the closure returns `None`.
    ///
    /// This operation cannot fail as the `MappedMutexGuard` passed
    /// in already locked the mutex.
    ///
    /// This is an associated function that needs to be
    /// used as `MappedMutexGuard::try_map(...)`. A method would interfere with methods of
    /// the same name on the contents of the locked data.
    pub fn try_map<U>(
        this: Self,
        f: impl FnOnce(&mut T) -> Option<&mut U>,
    ) -> Result<MappedMutexGuard<'a, U>, Self> {
        match f(unsafe { &mut *this.value }) {
            None => Err(this),
            Some(value) => {
                let lock = this.lock;
                mem::forget(this);
                Ok(MappedMutexGuard { lock, value })
            }
        }
    }

    /// Unlocks the mutex using a fair unlock protocol.
    ///
    /// By default, mutexes are unfair and allow the current future to re-lock
    /// the mutex before another has the chance to acquire the lock, even if
    /// that future has been blocked on the mutex for a long time. This can
    /// result in one future acquiring a mutex many more times than other futures.
    ///
    /// However in some cases it can be beneficial to ensure fairness by forcing
    /// the lock to pass on to a waiting thread if there is one. This is done by
    /// using this method instead of dropping the `MappedMutexGuard` normally.
    #[inline]
    pub fn unlock_fair(self) {
        if let Some(node) = self.lock.unlock_fair() {
            wake_node(node);
        }
        mem::forget(self);
    }
}
