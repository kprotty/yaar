#![allow(non_snake_case)]
#![allow(non_upper_case_globals)]

use crate::event::{YieldRequest, YieldResponse, OsInstant};
use core::{
    fmt,
    convert::TryInto,
    mem::{size_of, transmute},
    num::NonZeroUsize,
    ptr::null,
    sync::atomic::{spin_loop_hint, AtomicUsize, Ordering},
    time::Duration,
};
use yaar_sys::{
    CloseHandle, GetLastError, GetModuleHandleW, GetProcAddress, NtCreateKeyedEventFn,
    NtReleaseKeyedEventFn, NtWaitForKeyedEventFn,
    WaitOnAddressFn, WakeByAddressSingleFn, ERROR_TIMEOUT, FALSE,
    GENERIC_READ, GENERIC_WRITE, HANDLE, INFINITE, INVALID_HANDLE_VALUE, LARGE_INTEGER, PVOID,
    STATUS_SUCCESS, STATUS_TIMEOUT, TRUE,
};

const EMPTY: usize = 0;
const WAITING: usize = 1;
const NOTIFIED: usize = 2;

pub struct Signal {
    state: AtomicUsize,
}

impl fmt::Debug for Signal {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let state = match self.state.load(Ordering::SeqCst) {
            EMPTY => "empty",
            WAITING => "has_waiter",
            NOTIFIED => "notified",
            _ => "unknown",
        };

        f.debug_struct("OsSignal")
            .field("state", &state)
            .finish()
    }
}

impl Signal {
    pub const fn new() -> Self {
        Self {
            state: AtomicUsize::new(EMPTY),
        }
    }

    pub fn yield_now(request: YieldRequest) -> YieldResponse {
        match request {
            YieldRequest::QueryBestMethod => {
                YieldResponse::Block
            },
            YieldRequest::Spin { contended, iteration } => {
                spin_loop_hint();
                if !contended && iteration < 16 {
                    YieldResponse::Retry
                } else {
                    YieldResponse::Block
                }
            },
        }
    }

    pub fn notify(&self) {
        let mut state = self.state.load(Ordering::Relaxed);
        if state == EMPTY {
            state = self.state.compare_and_swap(EMPTY, NOTIFIED, Ordering::Relaxed);
            if state == EMPTY {
                return;
            }
        }

        debug_assert_eq!(state, WAITING);
        self.state.store(EMPTY, Ordering::Relaxed);
        unsafe { Backend::futex_wake(&self.state) };
    }

    pub fn wait(&self, timeout: Option<&mut Duration>) -> bool {
        let mut state = self.state.load(Ordering::Relaxed);
        if state == EMPTY {
            state = self.state.compare_and_swap(EMPTY, WAITING, Ordering::Relaxed);
            if state == EMPTY {
                return unsafe { 
                    Backend::futex_wait(&self.state, WAITING, EMPTY, timeout)
                };
            }
        }

        debug_assert_eq!(state, NOTIFIED);
        self.state.store(EMPTY, Ordering::Relaxed);
        true
    }
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
                if Self::load_wait_on_address() {
                    Self::WaitOnAddress
                } else if let Some(handle) = Self::load_keyed_event() {
                    Self::KeyedEvent(handle.get())
                } else {
                    unreachable!("OsAutoResetEvent requires either WaitOnAddress (Win8+) or NT Keyed Events (WinXP+)");
                }
            }
            WAIT_ON_ADDRESS => Self::WaitOnAddress,
            handle => Self::KeyedEvent(handle),
        }
    }

    pub unsafe fn futex_wake(ptr: &AtomicUsize) {
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

    pub unsafe fn futex_wait(
        ptr: &AtomicUsize,
        expect: usize,
        reset: usize,
        mut timeout: Option<&mut Duration>,
    ) -> bool {
        match Self::get() {
            Self::KeyedEvent(handle) => {
                let NtWaitForKeyedEvent = _NtWaitForKeyedEvent.load(Ordering::Relaxed);
                let NtWaitForKeyedEvent: NtWaitForKeyedEventFn = transmute(NtWaitForKeyedEvent);
                let timeout_end = timeout.as_ref().map(|t| OsInstant::now() + **t);

                let mut timeout_int = 1;
                let mut timeout_int_ptr = null();
                if let Some(timeout) = timeout.as_ref() {
                    timeout_int_ptr = &timeout_int;
                    timeout_int = -((*timeout).as_nanos() / 100)
                        .try_into()
                        .unwrap_or(LARGE_INTEGER::max_value());
                }

                let key = ptr as *const _ as PVOID;
                if timeout_int != 0 {
                    let status = NtWaitForKeyedEvent(handle, key, FALSE, timeout_int_ptr);
                    debug_assert!(status == STATUS_SUCCESS || status == STATUS_TIMEOUT);
                    if let (Some(timeout), Some(end)) = (timeout.as_mut(), timeout_end) {
                        **timeout = OsInstant::now().saturating_duration_since(end);
                    }
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
                let timeout_end = timeout.as_ref().map(|t| OsInstant::now() + **t);

                while ptr.load(Ordering::Acquire) == expect {
                    let timeout_ms = timeout
                        .as_ref()
                        .and_then(|t| (*t).as_millis().try_into().ok())
                        .unwrap_or(INFINITE);

                    if timeout_ms == 0 {
                        let timed_out = ptr.compare_and_swap(expect, reset, Ordering::Relaxed) == expect;
                        return !timed_out;
                    }
                
                    let status = WaitOnAddress(
                        ptr as *const _ as PVOID,
                        &expect as *const _ as PVOID,
                        size_of::<usize>(),
                        timeout_ms,
                    );

                    if let (Some(timeout), Some(end)) = (timeout.as_mut(), timeout_end) {
                        **timeout = OsInstant::now().saturating_duration_since(end);
                    }

                    debug_assert!(status == TRUE || status == FALSE);
                    if status == FALSE {
                        debug_assert_eq!(GetLastError(), ERROR_TIMEOUT);
                    }
                }

                true
            }
        }
    }

    #[cold]
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

    #[cold]
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