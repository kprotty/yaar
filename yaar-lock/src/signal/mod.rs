
pub unsafe trait Signal: Sync {
    #[cfg(feature = "condvar")]
    #[cfg_attr(feature = "nightly", doc(cfg(feature = "condvar")))]
    type Duration;

    fn wait(&self);

    #[cfg(feature = "condvar")]
    #[cfg_attr(feature = "nightly", doc(cfg(feature = "condvar")))]
    fn wait_timeout(&self, duration: Self::Duration) -> WaitTimeoutResult;

    fn notify(&self);

    #[cfg(feature = "condvar")]
    #[cfg_attr(feature = "nightly", doc(cfg(feature = "condvar")))]
    fn notify_all(&self);
}

#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum WaitTimeoutResult {
    Notified,
    TimedOut,
}

impl WaitTimeoutResult {
    #[inline]
    pub fn timed_out(&self) -> bool {
        self == Self::TimedOut
    }
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
        #[cfg(feature = "condvar")]
        type Duration = core::time::Duration;

        fn wait(&self) {
            self.inner.wait();
        }

        #[cfg(feature = "condvar")]
        fn wait_timeout(&self, duration: Self::Duration) -> WaitTimeoutResult {
            if self.inner.wait_timeout(duration) {
                WaitTimeoutResult::TimedOut
            } else {
                WaitTimeoutResult::Notified
            }
        }

        fn notify(&self) {
            self.inner.notify();
        }

        #[cfg(feature = "condvar")]
        fn notify_all(&self) {
            self.inner.notify_all();
        }
    }
}
