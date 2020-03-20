use super::sync;
use futures_intrusive::sync::*;

mod signal;
use signal::{GenericSignal, GenericSignalWaitFuture};

/// A synchronization primitive to block futures until notified
pub type RawSignal<Signal> = GenericSignal<sync::RawMutex<Signal>>;

/// A Future that is resolved once the corresponding `RawSinal` has been
/// notified
pub type RawSignalWaitFuture<'a, Signal> = GenericSignalWaitFuture<'a, sync::RawMutex<Signal>>;

/// A futures-aware mutex.
pub type RawMutex<T, Signal> = GenericMutex<sync::RawMutex<Signal>, T>;

/// An RAII guard returned by the lock and try_lock methods of [`RawMutex`].
/// When this structure is dropped (falls out of scope), the lock will be
/// unlocked.
pub type RawMutexGuard<'a, T, Signal> = GenericMutexGuard<'a, sync::RawMutex<Signal>, T>;

/// A future which resolves when the target mutex has been successfully
/// acquired.
pub type RawMutexLockFuture<'a, T, Signal> = GenericMutexLockFuture<'a, sync::RawMutex<Signal>, T>;

/// A futures-aware semaphore.
pub type RawSemaphore<Signal> = GenericSemaphore<sync::RawMutex<Signal>>;

/// An RAII guard returned by the acquire and try_acquire methods of
/// `RawSemaphore`.
pub type RawSemaphoreReleaser<'a, Signal> = GenericSemaphoreReleaser<'a, sync::RawMutex<Signal>>;

/// A future which resolves when the target semaphore has been successfully
/// acquired.
pub type RawSemaphoreAcquireFuture<'a, Signal> =
    GenericSemaphoreAcquireFuture<'a, sync::RawMutex<Signal>>;

#[cfg(feature = "os")]
use super::OsSignal;

/// A [`RawSignal`] backed by [`OsSignal`].
#[cfg(feature = "os")]
#[cfg_attr(feature = "nightly", doc(cfg(feature = "os")))]
pub type Signal = RawResetEvent<OsSignal>;

/// A [`RawSignalWaitFuture`] for [`Signal`].
#[cfg(feature = "os")]
#[cfg_attr(feature = "nightly", doc(cfg(feature = "os")))]
pub type SignalWaitFuture<'a> = RawSignalWaitFuture<'a, OsSignal>;

/// A [`RawMutex`] backed by [`OsSignal`].
#[cfg(feature = "os")]
#[cfg_attr(feature = "nightly", doc(cfg(feature = "os")))]
pub type Mutex<T> = RawMutex<T, OsSignal>;

/// A [`RawMutexGuard`] for [`Mutex`].
#[cfg(feature = "os")]
#[cfg_attr(feature = "nightly", doc(cfg(feature = "os")))]
pub type MutexGuard<'a, T> = RawMutexGuard<'a, T, OsSignal>;

/// A [`RawMutexLockFuture`] for [`Mutex`].
#[cfg(feature = "os")]
#[cfg_attr(feature = "nightly", doc(cfg(feature = "os")))]
pub type MutexLockFuture<'a, T> = RawMutexLockFuture<'a, T, OsSignal>;

/// A [`RawSemaphore`] backed by [`OsSignal`].
#[cfg(feature = "os")]
#[cfg_attr(feature = "nightly", doc(cfg(feature = "os")))]
pub type Semaphore = RawSemaphore<OsSignal>;

/// A [`RawSemaphoreReleaser`] for [`Semaphore`].
#[cfg(feature = "os")]
#[cfg_attr(feature = "nightly", doc(cfg(feature = "os")))]
pub type SemaphoreReleaser<'a> = RawSemaphoreReleaser<'a, OsSignal>;

/// A [`RawSemaphoreAcquireFuture`] for [`Semaphore`].
#[cfg(feature = "os")]
#[cfg_attr(feature = "nightly", doc(cfg(feature = "os")))]
pub type SemaphoreAcquireFuture<'a> = RawSemaphoreAcquireFuture<'a, OsSignal>;
