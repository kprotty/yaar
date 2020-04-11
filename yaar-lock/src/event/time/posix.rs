use super::OsDuration;
use core::mem::MaybeUninit;

pub struct Timer;

impl Timer {
    #[cfg(any(
        target_os = "ios",
        target_os = "macos",
        target_os = "fuschia",
        all(target_os = "linux", any(target_arch = "x86", target_arch = "x86_64")),
    ))]
    pub const IS_ACTUALLY_MONOTONIC: bool = true;

    #[cfg(not(any(
        target_os = "ios",
        target_os = "macos",
        target_os = "fuschia",
        all(target_os = "linux", any(target_arch = "x86", target_arch = "x86_64")),
    )))]
    pub const IS_ACTUALLY_MONOTONIC: bool = false;

    #[cfg(not(any(target_os = "macos", target_os = "ios")))]
    pub unsafe fn timestamp() -> OsDuration {
        use crate::utils::UnwrapUnchecked;
        use core::convert::TryInto;
        use yaar_sys::{clock_gettime, timespec, CLOCK_MONOTONIC};

        let mut now_ts = MaybeUninit::uninit();
        let status = clock_gettime(CLOCK_MONOTONIC, now_ts.as_mut_ptr());
        debug_assert_eq!(status, 0);

        let mut now_ts = now_ts.assume_init();
        OsDuration::new(
            now_ts.tv_sec.try_into().unwrap_unchecked(),
            now_ts.tv_nsec.try_into().unwrap_unchecked(),
        )
    }

    #[cfg(any(target_os = "macos", target_os = "ios"))]
    pub unsafe fn timestamp() -> OsDuration {
        #[derive(Copy, Clone)]
        struct mach_timebase_info {
            numer: u32,
            denom: u32,
        }
        extern "C" {
            fn mach_absolute_time() -> u64;
            fn mach_timebase_info(info: *mut mach_timebase_info) -> yaar_sys::c_int;
        }

        let info = {
            const UNINIT: usize = 0;
            const CREATING: usize = 1;
            const INIT: usize = 2;

            use core::sync::atomic::{AtomicUsize, Ordering};
            static STATE: AtomicUsize = AtomicUsize::new(0);
            static mut INFO: MaybeUninit<mach_timebase_info> = MaybeUninit::uninit();

            if STATE.load(Ordering::Acquire) == INIT {
                INFO.assume_init()
            } else {
                let mut info = MaybeUninit::uninit();
                let status = mach_timebase_info(info.as_mut_ptr());
                debug_assert_eq!(status, 0);
                if STATE.compare_and_swap(UNINIT, CREATING, Ordering::Relaxed) == UNINIT {
                    INFO = info;
                    STATE.store(INIT, Ordering::Release);
                }
                info.assume_init()
            }
        };

        const NANOS_PER_SEC: u64 = 1_000_000_000;
        let now = mach_absolute_time();
        let nanos = (now * info.numer) / info.denom;
        OsDuration::new(nanos / NANOS_PER_SEC, (nanos % NANOS_PER_SEC) as u32)
    }
}
