use super::OsDuration;
use crate::utils::UnwrapUnchecked;
use core::{
    convert::TryInto,
    sync::atomic::{AtomicUsize, Ordering},
};
use yaar_sys::{QueryPerformanceCounter, QueryPerformanceFrequency, LARGE_INTEGER, TRUE};

pub struct Timer;

impl Timer {
    pub const IS_ACTUALLY_MONOTONIC: bool = false;

    pub unsafe fn timestamp() -> OsDuration {
        let tick_resolution = {
            const UNINIT: usize = 0;
            const CREATING: usize = 1;
            const READY: usize = 2;

            static mut FREQUENCY: LARGE_INTEGER = 0;
            const NANOS_PER_SEC: LARGE_INTEGER = 1_000_000_000;
            static STATE: AtomicUsize = AtomicUsize::new(UNINIT);

            if STATE.load(Ordering::Acquire) == READY {
                FREQUENCY
            } else {
                let mut frequency = 0;
                let status = QueryPerformanceFrequency(&mut frequency);
                debug_assert_eq!(status, TRUE);
                frequency /= NANOS_PER_SEC / 100;

                if STATE.compare_and_swap(UNINIT, CREATING, Ordering::Relaxed) == UNINIT {
                    FREQUENCY = frequency;
                    STATE.store(READY, Ordering::Release);
                }

                frequency
            }
        };

        let mut ticks = 0;
        let status = QueryPerformanceCounter(&mut ticks);
        debug_assert_eq!(status, TRUE);

        ticks /= tick_resolution;
        let nanos: u64 = ticks.try_into().unwrap_unchecked();
        OsDuration::from_nanos(nanos * 100)
    }
}