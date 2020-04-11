pub struct YieldContext {
    pub contended: bool,
    pub iteration: usize,
    pub(crate) _sealed: (),
}

pub unsafe trait AutoResetEvent: Sync + Sized + Default {
    // Release barrier
    fn set(&self);

    // Acquire barrier
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
            let notified = self.signal.try_wait(None);
            debug_assert!(notified);
        }

        fn yield_now(context: YieldContext) -> bool {
            const SPINS: usize = 1024;
            const BITS: usize = (SPINS - 1).count_ones() as usize;

            if !context.contended {
                if context.iteration < BITS {
                    (0..(1 << context.iteration)).for_each(|_| spin_loop_hint());
                    return true;
                } else if context.iteration < BITS + 10 {
                    let spin = context.iteration - (BITS - 1);
                    (0..(spin * SPINS)).for_each(|_| spin_loop_hint());
                    return true;
                }
            }

            false
        }
    }

    unsafe impl AutoResetEventTimed for OsAutoResetEvent {
        type Instant = OsInstant;
        type Duration = OsDuration;

        fn try_wait_until(&self, timeout: Self::Instant) -> bool {
            self.signal.try_wait(Some(timeout))
        }

        fn try_wait_for(&self, timeout: &mut Self::Duration) -> bool {
            let times_out = OsInstant::now() + *timeout;
            let result = self.try_wait_until(times_out);
            *timeout = times_out.saturating_duration_since(OsInstant::now());
            result
        }
    }
}
