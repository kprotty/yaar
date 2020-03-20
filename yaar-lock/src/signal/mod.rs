/// Signal provides a basic mechanism to notify a single thread of an event.
/// This serves as the platform-dependent basis for implementing synchronization
/// primitives in yaar-lock.
///
/// A Signal should contain a single permit. `wait()` waits for the permit to be
/// made available, consumes the permit, and resumes. `notify()` sets the
/// permit, waking a pending thread if there was one.
///
/// If `notify()` is called **before** `wait()`, the next call to `wait()` will
/// complete immediately, consuming the permit. Any subsequent calls to `wait()`
/// will wait for a new permit.
///
/// If `notify()` is called **multiple** times before `wait()`, only a
/// **single** permit is stored. The next call to `wait()` will complete
/// immediately, but the one after will wait for a new permit.
pub unsafe trait Signal: Sync {
    /// Wait for a notification.
    ///
    /// The Signal should hold one permit. If the permit is available from an
    /// earlier call to `notify()`, the `wait()` will complete immediately,
    /// consuming that permit. Otherwise, `wait()` waits for the
    /// permit to be made available, blocking the current thread until the next
    /// call to `notify()`.
    ///
    /// This function assumes an [`Acquire`] memory ordering on completion.
    ///
    /// [`Acquire`]: `core::sync::atomic::Ordering::Acquire`
    fn wait(&self);

    /// Notifies a waiting thread.
    ///
    /// If a thread is current waiting for a permit, that thread is unblocked.
    /// Otherwise a permit is stored and the next call to `wait()` will complete
    /// immediately consuming the permit made available by this call to
    /// `notify()`.
    ///
    /// At most one permit may be stored by a `notify()` and sequential calls
    /// will result in a single permit being stored. The next call to
    /// `wait()` will complete immediately but the one after that will block.
    ///
    /// This function assumes a [`Release`] memory ordering on completion.
    ///
    /// [`Release`]: `core::sync::atomic::Ordering::Release`
    fn notify(&self);
}

#[cfg(all(feature = "os", target_os = "windows"))]
mod windows;
#[cfg(all(feature = "os", target_os = "windows"))]
use windows::SystemSignal;

#[cfg(all(feature = "os", target_os = "linux"))]
mod linux;
#[cfg(all(feature = "os", target_os = "linux"))]
use linux::SystemSignal;

#[cfg(all(feature = "os", unix, not(target_os = "linux")))]
mod posix;
#[cfg(all(feature = "os", unix, not(target_os = "linux")))]
use posix::SystemSignal;

#[cfg(feature = "os")]
pub use if_os::*;
#[cfg(feature = "os")]
mod if_os {
    use super::*;
    use core::fmt;

    /// A [`Signal`] implementation backed by OS thread blocking APIs
    #[cfg_attr(feature = "nightly", doc(cfg(feature = "os")))]
    pub struct OsSignal {
        inner: SystemSignal,
    }

    impl fmt::Debug for OsSignal {
        fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
            f.debug_struct("OsSignal").finish()
        }
    }

    impl Default for OsSignal {
        fn default() -> Self {
            Self::new()
        }
    }

    impl OsSignal {
        // Create an empty OsSignal with no waiters or notifications.
        pub const fn new() -> Self {
            Self {
                inner: SystemSignal::new(),
            }
        }
    }

    unsafe impl Send for OsSignal {}
    unsafe impl Sync for OsSignal {}

    unsafe impl Signal for OsSignal {
        fn wait(&self) {
            self.inner.wait();
        }

        fn notify(&self) {
            self.inner.notify();
        }
    }
}
