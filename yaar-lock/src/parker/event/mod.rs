pub unsafe trait AutoResetEvent: Sync + Default {
    fn set(&self);

    fn wait(&self);

    /// Hint to the platform that a thread is spinning, similar to
    /// [`spin_loop_hint`].
    ///
    /// Returns whether the thread should continue spinning or not.
    fn yield_now(iteration: usize) -> bool;
}

pub unsafe trait AutoResetEventTimed: AutoResetEvent {
    type Duration: Clone;

    fn try_wait(&self, timeout: Self::Duration) -> bool;
}

#[cfg(feature = "os")]
pub use if_os::*;

#[cfg(feature = "os")]
mod if_os {
    use super::*;
    use core::time::Duration;

    #[cfg_attr(windows, path = "../windows.rs")]
    mod os;

    pub struct OsAutoResetEvent {
        inner: os::AutoResetEvent,
    }

    unsafe impl Sync for OsAutoResetEvent {}
    unsafe impl Send for OsAutoResetEvent {}

    impl Default for OsAutoResetEvent {
        fn default() -> Self {
            Self::new()
        }
    }

    unsafe impl AutoResetEvent for OsAutoResetEvent {
        fn set(&self) {
            self.inner.set();
        }

        fn wait(&self) {
            let notified = self.inner.try_wait(None);
            debug_assert!(notified);
        }

        fn yield_now(iteration: usize) -> bool {
            os::AutoResetEvent::yield_now(iteration, Self::is_amd_ryzen())
        }
    }

    unsafe impl AutoResetEventTimed for OsAutoResetEvent {
        type Duration = Duration;

        fn try_wait(&self, timeout: Self::Duration) -> bool {
            self.inner.try_wait(Some(timeout))
        }
    }

    impl OsAutoResetEvent {
        pub fn new() -> Self {
            Self {
                inner: os::AutoResetEvent::new(),
            }
        }

        #[cfg(not(any(target_arch = "x86", target_arch = "x86_64")))]
        fn is_amd_ryzen() -> bool {
            false
        }

        #[cfg(any(target_arch = "x86", target_arch = "x86_64"))]
        fn is_amd_ryzen() -> bool {
            true
        }
    }
}