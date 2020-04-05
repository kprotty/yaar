
use super::monotonic::{Clock, ThreadLocalStorage};
use crate::utils::UnwrapUnchecked;
use core::{
    convert::TryInto,
    sync::atomic::{Ordering, AtomicUsize},
};

use yaar_sys::{GetLastError, ERROR_SUCCESS};
use yaar_sys::{QueryPerformanceCounter, QueryPerformanceFrequency, LARGE_INTEGER, TRUE};
use yaar_sys::{TlsAlloc, TlsFree, TlsGetValue, TlsSetValue, TLS_OUT_OF_INDEXES};

pub struct Timer;

unsafe impl ThreadLocalStorage for Timer {
    fn alloc() -> Option<usize> {
        match unsafe { TlsAlloc() } {
            TLS_OUT_OF_INDEXES => None,
            key => Some(key as usize),
        }
    }

    fn dealloc(key: usize) {
        unsafe {
            let key = key.try_into().unwrap_unchecked();
            let freed = TlsFree(key);
            debug_assert_eq!(freed, TRUE);
        }
    }

    fn get(key: usize) -> usize {
        unsafe {
            let key = key.try_into().unwrap_unchecked();
            let value = TlsGetValue(key);
            debug_assert_eq!(ERROR_SUCCESS, GetLastError());
            value
        }
    }

    fn set(key: usize, value: usize) {
        unsafe {
            let key = key.try_into().unwrap_unchecked();
            let stored = TlsSetValue(key, value);
            debug_assert_eq!(stored, TRUE);
        }
    }
}

const NANOS_PER_SEC: i64 = 1_000_000_000;
const NANOS_RESOLUTION: i64 = 100;

impl Clock for Timer {
    const IS_ALWAYS_MONOTONIC: bool = false;

    fn nanotime() -> u64 {
        unsafe {
            let ticks_per_sec = {
                const UNINIT: usize = 0;
                const CREATING: usize = 1;
                const READY: usize = 2;

                static mut FREQUENCY: LARGE_INTEGER = 0;
                static STATE: AtomicUsize = AtomicUsize::new(UNINIT);

                if STATE.load(Ordering::Acquire) == READY {
                    FREQUENCY
                } else {
                    let mut frequency = 0;
                    let status = QueryPerformanceFrequency(&mut frequency);
                    debug_assert_eq!(status, TRUE);

                    if STATE.compare_and_swap(UNINIT, CREATING, Ordering::Relaxed) == 0 {
                        FREQUENCY = frequency;
                        STATE.store(READY, Ordering::Release);
                    }

                    frequency
                }
            };

            let mut ticks = 0;
            let status = QueryPerformanceCounter(&mut ticks);
            debug_assert_eq!(status, TRUE);

            let resolution = NANOS_PER_SEC / NANOS_RESOLUTION;
            (ticks / (ticks_per_sec / resolution)) as u64
        }
    }

}