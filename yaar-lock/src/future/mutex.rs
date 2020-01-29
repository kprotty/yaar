// Documentation lifted from futures-intrusive and lock_api

use super::{WaitNode, WaitNodeState};
use futures_core::future::FusedFuture;
use core::{
    fmt,
    pin::Pin,
    future::Future,
    cell::UnsafeCell,
    ops::{Deref, DerefMut},
    task::{Poll, Context},
};

const LOCK_BIT: usize = 0b1;

/// A futures-aware mutex backed by os blocking.
#[cfg(feature = "os")]
#[cfg_attr(feature = "nightly", doc(cfg(feature = "os")))]
pub type Mutex<T> = RawMutex<T, crate::sync::WordLock<crate::OsThreadEvent>>;

/// A futures-aware mutex backed by os blocking.
#[cfg(feature = "os")]
#[cfg_attr(feature = "nightly", doc(cfg(feature = "os")))]
pub type MutexGuard<'a, T> = RawMutexGuard<'a, T, crate::sync::WordLock<crate::OsThreadEvent>>;

/// A futures-aware mutex backed by os blocking.
#[cfg(feature = "os")]
#[cfg_attr(feature = "nightly", doc(cfg(feature = "os")))]
pub type MutexLockFuture<'a, T> = RawMutexLockFuture<'a, T, crate::sync::WordLock<crate::OsThreadEvent>>;

/// A futures-aware mutex.
pub struct RawMutex<T, Raw: lock_api::RawMutex> {
    state: lock_api::Mutex<Raw, usize>, 
    value: UnsafeCell<T>,
}

// It is safe to send mutexes between threads, as long as they are not used and
// thereby borrowed
unsafe impl<T: Send, Raw: lock_api::RawMutex + Send> Send for RawMutex<T, Raw> {}

// The mutex is thread-safe as long as the utilized mutex is thread-safe
unsafe impl<T: Send, Raw: lock_api::RawMutex + Sync> Sync for RawMutex<T, Raw> {}

impl<T: fmt::Debug, Raw: lock_api::RawMutex> fmt::Debug for RawMutex<T, Raw> {
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

impl<T, Raw: lock_api::RawMutex> From<T> for RawMutex<T, Raw> {
    fn from(value: T) -> Self {
        Self::new(value)
    }
}

impl<T: Default, Raw: lock_api::RawMutex> Default for RawMutex<T, Raw> {
    fn default() -> Self {
        Self::new(T::default())
    }
}

impl<T, Raw: lock_api::RawMutex> RawMutex<T, Raw> {
    /// Creates a new mutex in an unlocked state ready for use.
    #[cfg(feature = "nightly")]
    #[inline]
    pub const fn new(value: T) -> Self {
        Self {
            state: lock_api::Mutex::new(0),
            value: UnsafeCell::new(value),
        }
    }

    /// Creates a new mutex in an unlocked state ready for use.
    #[cfg(not(feature = "nightly"))]
    #[inline]
    pub fn new(value: T) -> Self {
        Self {
            state: lock_api::Mutex::new(0),
            value: UnsafeCell::new(value),
        }
    }

    /// Consumes this mutex, returning the underlying data.
    #[inline]
    pub fn into_inner(self) -> T {
        self.value.into_inner()
    }

    /// Returns a mutable reference to the underlying data.
    ///
    /// Since this call borrows the `Mutex` mutably, no actual locking needs to
    /// take place---the mutable borrow statically guarantees no locks exist.
    #[inline]
    pub fn get_mut(&mut self) -> &mut T {
        unsafe { &mut *self.value.get() }
    }

    /// Attempts to acquire this lock.
    ///
    /// If the lock could not be acquired at this time, then `None` is returned.
    /// Otherwise, an RAII guard is returned. The lock will be unlocked when the
    /// guard is dropped.
    ///
    /// This function does not block.
    #[inline]
    pub fn try_lock(&self) -> Option<RawMutexGuard<'_, T, Raw>> {
        self.state.try_lock().and_then(|mut state| {
            if *state & LOCK_BIT == 0 {
                *state |= LOCK_BIT;
                Some(RawMutexGuard { mutex: self })
            } else {
                None
            }
        })
    }

    /// Acquire the mutex asynchronously.
    ///
    /// This method returns a future that will resolve once the mutex has been
    /// successfully acquired.
    #[inline]
    pub fn lock(&self) -> RawMutexLockFuture<'_, T, Raw> {
        RawMutexLockFuture {
            mutex: Some(self),
            wait_node: WaitNode::default(),
        }
    }
}

/// A future which resolves when the target mutex has been successfully acquired.
#[must_use = "futures do nothing unless polled"]
pub struct RawMutexLockFuture<'a, T, Raw: lock_api::RawMutex> {
    mutex: Option<&'a RawMutex<T, Raw>>,
    wait_node: WaitNode,
}

// Safety: Futures can be sent between threads as long as the underlying
// mutex is thread-safe (Sync), which allows to poll/register/unregister from
// a different thread.
unsafe impl<'a, T, Raw: lock_api::RawMutex + Sync> Send for RawMutexLockFuture<'a, T, Raw> {}

impl<'a, T: fmt::Debug, Raw: lock_api::RawMutex> fmt::Debug for RawMutexLockFuture<'a, T, Raw> {
    fn fmt(&self, f: &mut core::fmt::Formatter) -> core::fmt::Result {
        f.debug_struct("MutexLockFuture").finish()
    }
}

impl<'a, T, Raw: lock_api::RawMutex> FusedFuture for RawMutexLockFuture<'a, T, Raw> {
    fn is_terminated(&self) -> bool {
        self.mutex.is_none() 
    }
}

impl<'a, T, Raw: lock_api::RawMutex> Drop for RawMutexLockFuture<'a, T, Raw> {
    fn drop(&mut self) {
        if let Some(mutex) = self.mutex.take() {
            let mut state = mutex.state.lock();
            if self.wait_node.state.get() == WaitNodeState::Waiting {
                let head = (*state & !LOCK_BIT) as *const WaitNode;
                let new_head = self.wait_node.remove(head);
                *state = (new_head as usize) | (*state & LOCK_BIT);
            }
        }
    }
}

impl<'a, T, Raw: lock_api::RawMutex> Future for RawMutexLockFuture<'a, T, Raw> {
    type Output = RawMutexGuard<'a, T, Raw>;

    fn poll(self: Pin<&mut Self>, ctx: &mut Context<'_>) -> Poll<Self::Output> {
        // Safe as RawMutexLockFuture is !Unpin because of the WaitNode
        // and the address of the future wont change since its pinned. 
        let mut_self = unsafe { self.get_unchecked_mut() };
        let mutex = mut_self.mutex.expect("polled MutexLockFuture after completion");
        let mut state = mutex.state.lock();

        match mut_self.wait_node.state.get() {
            WaitNodeState::Reset => {
                if *state & LOCK_BIT == 0 {
                    *state |= LOCK_BIT;
                    mut_self.mutex = None;
                    Poll::Ready(RawMutexGuard { mutex })
                } else {
                    let head = (*state & !LOCK_BIT) as *const WaitNode;
                    let new_head = mut_self.wait_node.enqueue(head, ctx.waker().clone());
                    *state = (new_head as usize) | LOCK_BIT;
                    Poll::Pending
                }
            },
            WaitNodeState::Waiting => Poll::Pending,
            WaitNodeState::Notified => {
                mut_self.mutex = None;
                Poll::Ready(RawMutexGuard { mutex })
            }
        }
    }
}

/// An RAII implementation of a "scoped lock" of a mutex. When this structure is
/// dropped (falls out of scope), the lock will be unlocked.
///
/// The data protected by the mutex can be accessed through this guard via its
/// `Deref` and `DerefMut` implementations.
#[must_use = "if unused the Mutex will immediately unlock"]
pub struct RawMutexGuard<'a, T, Raw: lock_api::RawMutex> {
    mutex: &'a RawMutex<T, Raw>,
}

impl<'a, T, Raw: lock_api::RawMutex> DerefMut for RawMutexGuard<'a, T, Raw> {
    fn deref_mut(&mut self) -> &mut T {
        unsafe { &mut *self.mutex.value.get() }
    }
}

impl<'a, T, Raw: lock_api::RawMutex> Deref for RawMutexGuard<'a, T, Raw> {
    type Target = T;

    fn deref(&self) -> &T {
        unsafe { &*self.mutex.value.get() }
    }
}

impl<'a, T, Raw: lock_api::RawMutex> Drop for RawMutexGuard<'a, T, Raw> {
    fn drop(&mut self) {
        unlock(&mut self.mutex.state.lock(), false)
    }
}

impl<'a, T, Raw: lock_api::RawMutex> RawMutexGuard<'a, T, Raw> {
    /// Unlocks the mutex using a fair unlock protocol.
    ///
    /// By default, mutexes are unfair and allow the current future to re-lock
    /// the mutex before another has the chance to acquire the lock, even if
    /// that future has been blocked on the mutex for a long time. This is the
    /// default because it allows much higher throughput as it avoids forcing a
    /// context switch on every mutex unlock. This can result in one future
    /// acquiring a mutex many more times than other futures.
    ///
    /// However in some cases it can be beneficial to ensure fairness by forcing
    /// the lock to pass on to a waiting future if there is one. This is done by
    /// using this method instead of dropping the [`RawMutexGuard`] normally.
    pub fn unlock_fair(s: Self) {
        unlock(&mut s.mutex.state.lock(), true);
        core::mem::forget(s);
    }
}

/// Generic method for unlocking the mutex
fn unlock(state: &mut usize, is_fair: bool) {
    let head = (*state & !LOCK_BIT) as *const WaitNode;
    if head.is_null() {
        *state = 0;
    } else {
        let (new_head, tail) = unsafe { (&*head).dequeue() };
        *state = (new_head as usize) | (if is_fair { LOCK_BIT } else { 0 });
        tail.notify(is_fair)
    }
}