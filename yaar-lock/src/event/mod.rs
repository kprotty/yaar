
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
    use core::sync::atomic::spin_loop_hint;

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
            if Self::is_amd() || context.contended || context.iteration >= 1024 {
                spin_loop_hint();
                return false;
            }
            for _ in 0..(1 << context.iteration).min(100) {
                spin_loop_hint();
            }
            true
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

    impl OsAutoResetEvent {
        pub fn is_amd_ryzen() -> bool {
            Self::is_amd()
        }

        #[cfg(not(any(target_arch = "x86", target_arch = "x86_64")))]
        fn is_amd() -> bool {
            false
        }

        #[cfg(any(target_arch = "x86", target_arch = "x86_64"))]
        fn is_amd() -> bool {
            #[cfg(target_arch = "x86")]
            use core::arch::x86::{__cpuid, CpuidResult};
            #[cfg(target_arch = "x86_64")]
            use core::arch::x86_64::{__cpuid, CpuidResult};

            use core::{
                slice::from_raw_parts,
                str::from_utf8_unchecked,
                hint::unreachable_unchecked,
                sync::atomic::{AtomicUsize, Ordering},
            };

            static IS_AMD: AtomicUsize = AtomicUsize::new(0);
            unsafe {
                match IS_AMD.load(Ordering::Relaxed) {
                    0 => {
                        let CpuidResult { ebx, ecx, edx, .. } = __cpuid(0);
                        let vendor = &[ebx, edx, ecx] as *const _ as *const u8;
                        let vendor = from_utf8_unchecked(from_raw_parts(vendor, 3 * 4));
                        let is_amd = vendor == "AuthenticAMD";
                        IS_AMD.store((is_amd as usize) + 1, Ordering::Relaxed);
                        is_amd
                    },
                    1 => false,
                    2 => true,
                    _ => unreachable_unchecked(),
                }
            }
        }
    }
}