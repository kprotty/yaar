pub struct YieldContext {
    pub contended: bool,
    pub iteration: usize,
    pub(crate) _sealed: (),
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

#[cfg(all(feature = "os", windows))]
mod windows;
#[cfg(all(feature = "os", windows))]
use windows::Signal;

#[cfg(all(feature = "os", unix))]
mod posix;
#[cfg(all(feature = "os", unix))]
use posix::Signal;

#[cfg(feature = "os")]
mod time;
#[cfg(feature = "os")]
pub use time::*;

#[cfg(feature = "os")]
pub use if_os::*;
#[cfg(feature = "os")]
mod if_os {
    use super::*;
    use core::sync::atomic::spin_loop_hint;

    #[derive(Debug)]
    pub struct OsAutoResetEvent {
        signal: Signal,
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
                signal: Signal::new(),
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
            const BITS: usize = (1024usize - 1).count_ones() as usize;

            if !context.contended {
                if context.iteration < BITS {
                    (0..(1 << context.iteration)).for_each(|_| spin_loop_hint());
                    return true;
                } else if context.iteration < BITS + 10 {
                    let spin = context.iteration - (BITS - 1);
                    (0..(spin * 1024)).for_each(|_| spin_loop_hint());
                    return true;
                }
            }

            false
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
