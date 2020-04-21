use core::mem::MaybeUninit;

pub struct Timer;

impl Timer {
    /// According to std::time::Instant::now(), these platforms have been
    /// observed to report monotonic clocks which go backwards in time.
    #[cfg(any(
        all(target_os = "openbsd", target_arch = "x86_64"),
        all(
            target_os = "linux",
            any(target_arch = "aarch64", target_arch = "s390x")
        ),
    ))]
    pub const IS_ACTUALLY_MONOTONIC: bool = false;

    #[cfg(not(any(
        all(target_os = "openbsd", target_arch = "x86_64"),
        all(
            target_os = "linux",
            any(target_arch = "aarch64", target_arch = "s390x")
        ),
    )))]
    pub const IS_ACTUALLY_MONOTONIC: bool = true;

    /// Get the current timestamp as reported by the OS in nanoseconds for
    /// non-darwin systems.
    #[cfg(not(any(target_os = "macos", target_os = "ios")))]
    pub unsafe fn timestamp() -> u64 {
        const NANOS_PER_SEC: u64 = 1_000_000_000;

        use crate::utils::UnwrapUnchecked;
        use core::convert::TryInto;
        use yaar_sys::{clock_gettime, CLOCK_MONOTONIC};

        let mut now_ts = MaybeUninit::uninit();
        let status = clock_gettime(CLOCK_MONOTONIC, now_ts.as_mut_ptr());
        debug_assert_eq!(status, 0);

        let now_ts = now_ts.assume_init();
        let secs: u64 = now_ts.tv_sec.try_into().unwrap_unchecked();
        let nsecs: u64 = now_ts.tv_nsec.try_into().unwrap_unchecked();
        nsecs + (secs * NANOS_PER_SEC)
    }

    /// Get the current timestamp as reported by the OS in nanoseconds for
    /// darwin-based systems.
    #[cfg(any(target_os = "macos", target_os = "ios"))]
    pub unsafe fn timestamp() -> u64 {
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

        let now = mach_absolute_time();
        (now * info.numer) / info.denom
    }
}
