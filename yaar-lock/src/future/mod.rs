use super::sync::WordLock;
use futures_intrusive::sync::*;

/// A synchronization primitive which can be either in the set or reset state.
pub type RawResetEvent<ThreadEvent> = GenericManualResetEvent<WordLock<ThreadEvent>>;

/// A Future that is resolved once the corresponding `RawResetEvent` has been
/// set
pub type RawWaitForEventFuture<'a, ThreadEvent> =
    GenericWaitForEventFuture<'a, WordLock<ThreadEvent>>;

/// A futures-aware mutex.
pub type RawMutex<T, ThreadEvent> = GenericMutex<WordLock<ThreadEvent>, T>;

/// An RAII guard returned by the lock and try_lock methods of `RawMutex`.
/// When this structure is dropped (falls out of scope), the lock will be
/// unlocked.
pub type RawMutexGuard<'a, T, ThreadEvent> = GenericMutexGuard<'a, WordLock<ThreadEvent>, T>;

/// A future which resolves when the target mutex has been successfully
/// acquired.
pub type RawMutexLockFuture<'a, T, ThreadEvent> =
    GenericMutexLockFuture<'a, WordLock<ThreadEvent>, T>;

/// A futures-aware semaphore.
pub type RawSemaphore<ThreadEvent> = GenericSemaphore<WordLock<ThreadEvent>>;

/// An RAII guard returned by the acquire and try_acquire methods of
/// `RawSemaphore`.
pub type RawSemaphoreReleaser<'a, ThreadEvent> =
    GenericSemaphoreReleaser<'a, WordLock<ThreadEvent>>;

/// A future which resolves when the target semaphore has been successfully
/// acquired.
pub type RawSemaphoreAcquireFuture<'a, ThreadEvent> =
    GenericSemaphoreAcquireFuture<'a, WordLock<ThreadEvent>>;

#[cfg(feature = "os")]
use super::OsThreadEvent;

/// A [`RawResetEvent`] backed by [`OsThreadEvent`].
#[cfg(feature = "os")]
#[cfg_attr(feature = "nightly", doc(cfg(feature = "os")))]
pub type ResetEvent = RawResetEvent<OsThreadEvent>;

/// A [`RawWaitForEventFuture`] for [`ResetEvent`].
#[cfg(feature = "os")]
#[cfg_attr(feature = "nightly", doc(cfg(feature = "os")))]
pub type WaitForEventFuture<'a> = RawWaitForEventFuture<'a, OsThreadEvent>;

/// A [`RawMutex`] backed by [`OsThreadEvent`].
#[cfg(feature = "os")]
#[cfg_attr(feature = "nightly", doc(cfg(feature = "os")))]
pub type Mutex<T> = RawMutex<T, OsThreadEvent>;

/// A [`RawMutexGuard`] for [`Mutex`].
#[cfg(feature = "os")]
#[cfg_attr(feature = "nightly", doc(cfg(feature = "os")))]
pub type MutexGuard<'a, T> = RawMutexGuard<'a, T, OsThreadEvent>;

/// A [`RawMutexLockFuture`] for [`Mutex`].
#[cfg(feature = "os")]
#[cfg_attr(feature = "nightly", doc(cfg(feature = "os")))]
pub type MutexLockFuture<'a, T> = RawMutexLockFuture<'a, T, OsThreadEvent>;

/// A [`RawSemaphore`] backed by [`OsThreadEvent`].
#[cfg(feature = "os")]
#[cfg_attr(feature = "nightly", doc(cfg(feature = "os")))]
pub type Semaphore = RawSemaphore<OsThreadEvent>;

/// A [`RawSemaphoreReleaser`] for [`Semaphore`].
#[cfg(feature = "os")]
#[cfg_attr(feature = "nightly", doc(cfg(feature = "os")))]
pub type SemaphoreReleaser<'a> = RawSemaphoreReleaser<'a, OsThreadEvent>;

/// A [`RawSemaphoreAcquireFuture`] for [`Semaphore`].
#[cfg(feature = "os")]
#[cfg_attr(feature = "nightly", doc(cfg(feature = "os")))]
pub type SemaphoreAcquireFuture<'a> = RawSemaphoreAcquireFuture<'a, OsThreadEvent>;
