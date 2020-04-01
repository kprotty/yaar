#![allow(non_snake_case)]
#![allow(non_upper_case_globals)]

use core::{
    convert::TryInto,
    mem::{size_of, transmute},
    num::NonZeroUsize,
    ptr::null,
    sync::atomic::{spin_loop_hint, AtomicUsize, Ordering},
    time::Duration,
};
use yaar_sys::{
    CloseHandle, GetLastError, GetModuleHandleW, GetProcAddress, NtCreateKeyedEventFn,
    NtReleaseKeyedEventFn, NtWaitForKeyedEventFn, QueryPerformanceCounter,
    QueryPerformanceFrequency, WaitOnAddressFn, WakeByAddressSingleFn, ERROR_TIMEOUT, FALSE,
    GENERIC_READ, GENERIC_WRITE, HANDLE, INFINITE, INVALID_HANDLE_VALUE, LARGE_INTEGER, PVOID,
    STATUS_SUCCESS, STATUS_TIMEOUT, TRUE,
};

pub fn yield_now(iteration: usize) -> bool {
    spin_loop_hint();
    iteration < 40
}

pub unsafe fn futex_wake(ptr: &AtomicUsize) {
    Backend::wake(ptr)
}

pub unsafe fn futex_wait(
    ptr: &AtomicUsize,
    expect: usize,
    reset: usize,
    timeout: Option<Duration>,
) -> bool {
    Backend::wait(ptr, expect, reset, timeout)
}

pub unsafe fn timestamp() -> Duration {
    let frequency = {
        static mut FREQUENCY: LARGE_INTEGER = 0;
        static STATE: AtomicUsize = AtomicUsize::new(0);

        if STATE.load(Ordering::Acquire) == 2 {
            FREQUENCY
        } else {
            let mut frequency = 0;
            let status = QueryPerformanceFrequency(&mut frequency);
            debug_assert_eq!(status, TRUE);

            if STATE.compare_and_swap(0, 1, Ordering::Relaxed) == 0 {
                FREQUENCY = frequency;
                STATE.store(2, Ordering::Release);
            }

            frequency
        }
    };

    let mut counter = 0;
    let status = QueryPerformanceCounter(&mut counter);
    debug_assert_eq!(status, TRUE);

    const NANOS_PER_SEC: i64 = 1_000_000_000;
    let ns = counter / (frequency / (NANOS_PER_SEC / 100));
    Duration::from_nanos(ns as u64)
}

const WAIT_ON_ADDRESS: usize = INVALID_HANDLE_VALUE;
static HANDLE: AtomicUsize = AtomicUsize::new(0);

static _NtWaitForKeyedEvent: AtomicUsize = AtomicUsize::new(0);
static _NtReleaseKeyedEvent: AtomicUsize = AtomicUsize::new(0);

static _WaitOnAddress: AtomicUsize = AtomicUsize::new(0);
static _WakeByAddressSingle: AtomicUsize = AtomicUsize::new(0);

enum Backend {
    WaitOnAddress,
    KeyedEvent(HANDLE),
}

impl Backend {
    unsafe fn get() -> Self {
        match HANDLE.load(Ordering::Acquire) {
            0 => {
                if let Some(handle) = Self::load_keyed_event() {
                    Self::KeyedEvent(handle.get())
                } else if Self::load_wait_on_address() {
                    Self::WaitOnAddress
                } else {
                    unreachable!("OsAutoResetEvent requires either WaitOnAddress (Win8+) or NT Keyed Events (WinXP+)");
                }
            }
            WAIT_ON_ADDRESS => Self::WaitOnAddress,
            handle => Self::KeyedEvent(handle),
        }
    }

    pub unsafe fn wake(ptr: &AtomicUsize) {
        match Self::get() {
            Self::KeyedEvent(handle) => {
                let NtReleaseKeyedEvent = _NtReleaseKeyedEvent.load(Ordering::Relaxed);
                let NtReleaseKeyedEvent: NtReleaseKeyedEventFn = transmute(NtReleaseKeyedEvent);
                let status = NtReleaseKeyedEvent(handle, ptr as *const _ as PVOID, FALSE, null());
                debug_assert_eq!(status, STATUS_SUCCESS);
            }
            Self::WaitOnAddress => {
                let WakeByAddressSingle = _WakeByAddressSingle.load(Ordering::Relaxed);
                let WakeByAddressSingle: WakeByAddressSingleFn = transmute(WakeByAddressSingle);
                WakeByAddressSingle(ptr as *const _ as PVOID);
            }
        }
    }

    pub unsafe fn wait(
        ptr: &AtomicUsize,
        expect: usize,
        reset: usize,
        mut timeout: Option<Duration>,
    ) -> bool {
        match Self::get() {
            Self::KeyedEvent(handle) => {
                let NtWaitForKeyedEvent = _NtWaitForKeyedEvent.load(Ordering::Relaxed);
                let NtWaitForKeyedEvent: NtWaitForKeyedEventFn = transmute(NtWaitForKeyedEvent);

                let mut timeout_int = 1;
                let mut timeout_int_ptr = null();
                if let Some(timeout) = timeout {
                    timeout_int_ptr = &timeout_int;
                    timeout_int = -(timeout.as_nanos() / 100)
                        .try_into()
                        .unwrap_or(LARGE_INTEGER::max_value());
                }

                let key = ptr as *const _ as PVOID;
                if timeout_int != 0 {
                    let status = NtWaitForKeyedEvent(handle, key, FALSE, timeout_int_ptr);
                    debug_assert!(status == STATUS_SUCCESS || status == STATUS_TIMEOUT);
                    if status == STATUS_SUCCESS {
                        return true;
                    }
                }

                if ptr.load(Ordering::Relaxed) == expect {
                    if ptr.compare_and_swap(expect, reset, Ordering::Relaxed) == expect {
                        return false;
                    }
                }

                let status = NtWaitForKeyedEvent(handle, key, FALSE, null());
                debug_assert_eq!(status, STATUS_SUCCESS);
                true
            }
            Self::WaitOnAddress => {
                let WaitOnAddress = _WaitOnAddress.load(Ordering::Relaxed);
                let WaitOnAddress: WaitOnAddressFn = transmute(WaitOnAddress);
                let timeout_timestamp = timeout.map(|t| timestamp() + t);

                while ptr.load(Ordering::Acquire) == expect {
                    let timeout_ms = timeout
                        .map(|t| t.as_millis().try_into().unwrap_or(INFINITE - 1))
                        .unwrap_or(INFINITE);

                    if timeout_ms == 0 {
                        if ptr.load(Ordering::Acquire) == expect {
                            if ptr.compare_and_swap(expect, reset, Ordering::Relaxed) == expect {
                                return false;
                            }
                        }
                        return true;
                    }

                    let status = WaitOnAddress(
                        ptr as *const _ as PVOID,
                        &expect as *const _ as PVOID,
                        size_of::<usize>(),
                        timeout_ms,
                    );

                    debug_assert!(status == TRUE || status == FALSE);
                    if status == FALSE {
                        debug_assert_eq!(GetLastError(), ERROR_TIMEOUT);
                        if let Some(t) = timeout_timestamp {
                            let t = t.checked_sub(timestamp());
                            let t = t.unwrap_or(Duration::from_secs(0));
                            timeout = Some(t);
                        }
                    }
                }

                true
            }
        }
    }

    unsafe fn load_wait_on_address() -> bool {
        let dll = GetModuleHandleW(
            (&[
                b'a' as u16,
                b'p' as u16,
                b'i' as u16,
                b'-' as u16,
                b'm' as u16,
                b's' as u16,
                b'-' as u16,
                b'w' as u16,
                b'i' as u16,
                b'n' as u16,
                b'-' as u16,
                b'c' as u16,
                b'o' as u16,
                b'r' as u16,
                b'e' as u16,
                b'-' as u16,
                b's' as u16,
                b'y' as u16,
                b'n' as u16,
                b'c' as u16,
                b'h' as u16,
                b'-' as u16,
                b'l' as u16,
                b'1' as u16,
                b'-' as u16,
                b'2' as u16,
                b'-' as u16,
                b'0' as u16,
                b'.' as u16,
                b'd' as u16,
                b'l' as u16,
                b'l' as u16,
                0 as u16,
            ])
                .as_ptr(),
        );
        if dll == 0 {
            return false;
        }

        let wake = GetProcAddress(dll, b"WakeByAddressSingle\0".as_ptr());
        if wake == 0 {
            return false;
        } else {
            _WakeByAddressSingle.store(wake, Ordering::Relaxed);
        }

        let wait = GetProcAddress(dll, b"WaitOnAddress\0".as_ptr());
        if wait == 0 {
            return false;
        } else {
            _WaitOnAddress.store(wait, Ordering::Relaxed);
        }

        HANDLE.store(WAIT_ON_ADDRESS, Ordering::Release);
        true
    }

    unsafe fn load_keyed_event() -> Option<NonZeroUsize> {
        let dll = GetModuleHandleW(
            (&[
                b'n' as u16,
                b't' as u16,
                b'd' as u16,
                b'l' as u16,
                b'l' as u16,
                b'.' as u16,
                b'd' as u16,
                b'l' as u16,
                b'l' as u16,
                0 as u16,
            ])
                .as_ptr(),
        );
        if dll == 0 {
            return None;
        }

        let release = GetProcAddress(dll, b"NtReleaseKeyedEvent\0".as_ptr());
        if release == 0 {
            return None;
        } else {
            _NtReleaseKeyedEvent.store(release, Ordering::Relaxed);
        }

        let wait = GetProcAddress(dll, b"NtWaitForKeyedEvent\0".as_ptr());
        if wait == 0 {
            return None;
        } else {
            _NtWaitForKeyedEvent.store(wait, Ordering::Relaxed);
        }

        let create = GetProcAddress(dll, b"NtCreateKeyedEvent\0".as_ptr());
        if create == 0 {
            return None;
        }

        let mut handle = INVALID_HANDLE_VALUE;
        let create: NtCreateKeyedEventFn = transmute(create);
        if create(&mut handle, GENERIC_READ | GENERIC_WRITE, 0, 0) != STATUS_SUCCESS {
            return None;
        }

        let new_handle = HANDLE.compare_and_swap(0, handle, Ordering::Release);
        if new_handle != 0 {
            let closed = CloseHandle(handle);
            debug_assert_eq!(closed, TRUE);
            handle = new_handle;
        }

        NonZeroUsize::new(handle)
    }
}
