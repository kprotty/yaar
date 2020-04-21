use crate::utils::UnwrapUnchecked;
use core::{
    convert::TryInto,
    sync::atomic::{AtomicUsize, Ordering},
};
use yaar_sys::{QueryPerformanceCounter, QueryPerformanceFrequency, LARGE_INTEGER, TRUE};

pub struct Timer;

impl Timer {
    /// According to std::time::Instant, these windows platforms appear to not
    /// guarantee monotonically increasing time.
    #[cfg(any(target_arch = "x86", target_arch = "x86_64"))]
    pub const IS_ACTUALLY_MONOTONIC: bool = false;

    #[cfg(not(any(target_arch = "x86", target_arch = "x86_64")))]
    pub const IS_ACTUALLY_MONOTONIC: bool = true;

    /// Get the current timestamp as reported by the OS.
    pub unsafe fn timestamp() -> u64 {
        let tick_resolution = {
            const UNINIT: usize = 0;
            const CREATING: usize = 1;
            const READY: usize = 2;

            static mut FREQUENCY: LARGE_INTEGER = 0;
            const NANOS_PER_SEC: LARGE_INTEGER = 1_000_000_000;
            static STATE: AtomicUsize = AtomicUsize::new(UNINIT);

            /// Cold path to create the tick frequency manually.
            ///
            /// Stores the frequency in units of ticks per nanoseconds instead
            /// of seconds to keep later conversion to a minimum.
            #[cold]
            unsafe fn get_frequency() -> LARGE_INTEGER {
                let mut frequency = 0;
                let status = QueryPerformanceFrequency(&mut frequency);
                debug_assert_eq!(status, TRUE);
                frequency /= NANOS_PER_SEC;

                if STATE.load(Ordering::Relaxed) == UNINIT {
                    if STATE.compare_and_swap(UNINIT, CREATING, Ordering::Relaxed) == UNINIT {
                        FREQUENCY = frequency;
                        STATE.store(READY, Ordering::Release);
                    }
                }

                frequency
            }

            // Load the frequency with assumed fast path
            if STATE.load(Ordering::Acquire) == READY {
                FREQUENCY
            } else {
                get_frequency()
            }
        };

        let mut ticks = 0;
        let status = QueryPerformanceCounter(&mut ticks);
        debug_assert_eq!(status, TRUE);

        ticks /= tick_resolution;
        ticks.try_into().unwrap_unchecked()
    }
}
