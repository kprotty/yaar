

pub struct YieldContext {
    pub contended: bool,
    pub iteration: usize,
}

pub unsafe trait AutoResetEvent: Sync + Sized + Default {
    fn set(&self);

    fn wait(&self);

    fn yield_now(context: YieldContext) -> bool;
}

pub unsafe trait AutoResetEventTimed: AutoResetEvent {
    type Instant;
    type Duration;

    fn try_wait_until(&self, timeout: Self::Instant) -> bool;

    fn try_wait_for(&self, timeout: &mut Self::Duration) -> bool;
}

#[cfg(feature = "os")]
#[cfg_attr(windows, path = "./windows.rs")]
mod os;

#[cfg(feature = "os")]
mod time;
#[cfg(feature = "os")]
pub use time::*;

#[cfg(feature = "os")]
pub use if_os::*;
#[cfg(feature = "os")]
mod if_os {
    use super::*;

    #[derive(Debug)]
    pub struct OsAutoResetEvent {
        signal: os::Signal,
    }

    unsafe impl Sync for OsAutoResetEvent {}
    unsafe impl Send for OsAutoResetEvent {}

    impl Default for OsAutoResetEvent {
        fn default() -> Self {
            Self::new()
        }
    }

    impl OsAutoResetEvent {
        pub const fn new() -> Self {
            Self {
                signal: os::Signal::new(),
            }
        }
    }

    unsafe impl AutoResetEvent for OsAutoResetEvent {
        fn set(&self) {
            self.signal.notify();
        }

        fn wait(&self) {
            let notified = self.signal.wait(None);
            debug_assert!(notified);
        }

        fn yield_now(context: YieldContext) -> bool {
            os::Signal::yield_now(context)
        }
    }

    unsafe impl AutoResetEventTimed for OsAutoResetEvent {
        type Instant = OsInstant;
        type Duration = OsDuration;

        fn try_wait_for(&self, timeout: &mut Self::Duration) -> bool {
            self.signal.wait(Some(timeout))
        }

        fn try_wait_until(&self, timeout: Self::Instant) -> bool {
            let mut timeout = OsInstant::now().saturating_duration_since(timeout);
            timeout.as_nanos() != 0 && self.try_wait_for(&mut timeout)
        }
    }    
}