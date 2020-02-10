#![allow(non_snake_case, non_upper_case_globals)]

use core::{
    mem::{size_of, transmute, MaybeUninit},
    ptr::null_mut,
    sync::atomic::{spin_loop_hint, AtomicU32, AtomicUsize, Ordering},
};
use winapi::{
    shared::{
        basetsd::SIZE_T,
        minwindef::{BOOL, DWORD, TRUE, ULONG},
        ntdef::{FALSE, NTSTATUS},
        ntstatus::STATUS_SUCCESS,
    },
    um::{
        handleapi::{CloseHandle, INVALID_HANDLE_VALUE},
        libloaderapi::{GetModuleHandleW, GetProcAddress},
        winbase::INFINITE,
        winnt::{
            ACCESS_MASK, BOOLEAN, GENERIC_READ, GENERIC_WRITE, HANDLE, LPCSTR, LPCWSTR, PHANDLE,
            PLARGE_INTEGER, PVOID,
        },
    },
};

const IS_RESET: u32 = 0;
const IS_SET: u32 = 1;
const IS_WAITING: u32 = 2;

pub struct Event {
    state: AtomicU32,
}

impl Default for Event {
    fn default() -> Self {
        Self {
            state: AtomicU32::new(IS_RESET),
        }
    }
}

impl Event {
    pub fn is_set(&self) -> bool {
        self.state.load(Ordering::Acquire) == IS_SET
    }

    pub fn reset(&self) {
        self.state.store(IS_RESET, Ordering::Relaxed);
    }

    pub fn set(&self) {
        let waiting = self.state.swap(IS_SET, Ordering::Release) >> 1;
        if waiting != 0 {
            let void_ptr = &self.state as *const _ as PVOID;
            match get_backend() {
                Backend::KeyedEvent(handle) => unsafe {
                    let notify = _NtReleaseKeyedEvent.load(Ordering::Relaxed);
                    let notify = transmute::<_, NtInvokeKeyedEvent>(notify);
                    for _ in 0..waiting {
                        let status = notify(handle, void_ptr, FALSE, null_mut());
                        debug_assert_eq!(status, STATUS_SUCCESS, "Error in NtReleaseKeyedEvent()");
                    }
                },
                Backend::WaitOnAddress => unsafe {
                    let notify = _WakeByAddressAll.load(Ordering::Relaxed);
                    let notify = transmute::<_, WakeByAddressAll>(notify);
                    notify(void_ptr);
                },
            }
        }
    }

    pub fn wait(&self) {
        // Try to transition into the waiting state, returning if already signaled.
        loop {
            let state = self.state.load(Ordering::Acquire);
            if state == IS_SET {
                return;
            }
            match self.state.compare_exchange_weak(
                state,
                state + IS_WAITING,
                Ordering::Acquire,
                Ordering::Relaxed,
            ) {
                Ok(_) => break,
                Err(_) => spin_loop_hint(),
            }
        }

        let void_ptr = &self.state as *const _ as PVOID;
        match get_backend() {
            Backend::KeyedEvent(handle) => unsafe {
                // WaitForKeyedEvent has no spurious wake-ups.
                let wait = _NtWaitForKeyedEvent.load(Ordering::Relaxed);
                let wait = transmute::<_, NtInvokeKeyedEvent>(wait);
                let status = wait(handle, void_ptr, FALSE, null_mut());
                debug_assert_eq!(status, STATUS_SUCCESS, "Error in NtWaitForKeyedEvent");
            },
            Backend::WaitOnAddress => unsafe {
                let compare_ptr = &IS_WAITING as *const _ as PVOID;
                let wait = _WaitOnAddress.load(Ordering::Relaxed);
                let wait = transmute::<_, WaitOnAddress>(wait);
                while self.state.load(Ordering::Acquire) != IS_SET {
                    let status = wait(void_ptr, compare_ptr, size_of::<Self>(), INFINITE);
                    debug_assert_eq!(status, TRUE, "Error in WaitOnAddress");
                }
            },
        }
    }
}

enum Backend {
    WaitOnAddress,
    KeyedEvent(HANDLE),
}

const WAIT_ON_ADDRESS: usize = !0;
static BACKEND_HANDLE: AtomicUsize = AtomicUsize::new(0);

/// Lazily initializes a windows backend for thread parking and notification.
fn get_backend() -> Backend {
    // Acquire orderings on both loads to observe the loaded functions.
    match BACKEND_HANDLE.load(Ordering::Acquire) {
        0 => unsafe {
            if load_keyed_events() {
                Backend::KeyedEvent(BACKEND_HANDLE.load(Ordering::Acquire) as HANDLE)
            } else if load_wait_on_address() {
                Backend::WaitOnAddress
            } else {
                unreachable!("Windows Event requires either WaitOnAddress (Win8+) or NT Keyed Events (WinXP+)")
            }
        },
        WAIT_ON_ADDRESS => Backend::WaitOnAddress,
        handle => Backend::KeyedEvent(handle as HANDLE),
    }
}

static _WakeByAddressAll: AtomicUsize = AtomicUsize::new(0);
type WakeByAddressAll = extern "stdcall" fn(Address: PVOID);

static _WaitOnAddress: AtomicUsize = AtomicUsize::new(0);
type WaitOnAddress = extern "stdcall" fn(
    Address: PVOID,
    CompareAddress: PVOID,
    AddressSize: SIZE_T,
    dwMilliseconds: DWORD,
) -> BOOL;

/// Try to load the WaitOnAddress api into the process.
/// On success, sets the necessary functions and returns true.
unsafe fn load_wait_on_address() -> bool {
    // api-ms-win-core-synch-l1-2-0.dll
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
            .as_ptr() as LPCWSTR,
    );
    if dll.is_null() {
        return false;
    }

    let wait = GetProcAddress(dll, b"WaitOnAddress\0".as_ptr() as LPCSTR);
    if wait.is_null() {
        return false;
    } else {
        _WaitOnAddress.store(wait as usize, Ordering::Relaxed);
    }

    let notify = GetProcAddress(dll, b"WakeByAddressAll\0".as_ptr() as LPCSTR);
    if notify.is_null() {
        return false;
    } else {
        _WakeByAddressAll.store(notify as usize, Ordering::Relaxed);
    }

    debug_assert_eq!(
        WAIT_ON_ADDRESS, INVALID_HANDLE_VALUE as usize,
        "incorrect value for WAIT_ON_ADDRESS"
    );

    // Use Release ordering so other threads see the loaded functions above.
    BACKEND_HANDLE.store(WAIT_ON_ADDRESS, Ordering::Release);
    true
}

static _NtReleaseKeyedEvent: AtomicUsize = AtomicUsize::new(0);
static _NtWaitForKeyedEvent: AtomicUsize = AtomicUsize::new(0);
type NtInvokeKeyedEvent = extern "stdcall" fn(
    EventHandle: HANDLE,
    Key: PVOID,
    Alertable: BOOLEAN,
    Timeout: PLARGE_INTEGER,
) -> NTSTATUS;

/// Try to load NT Keyed Events api into the process.
/// On success, stores the event handle in BACKEND_HANDLE and returns true.
unsafe fn load_keyed_events() -> bool {
    // ntdll.dll
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
            .as_ptr() as LPCWSTR,
    );
    if dll.is_null() {
        return false;
    }

    let wait = GetProcAddress(dll, b"NtWaitForKeyedEvent\0".as_ptr() as LPCSTR);
    if wait.is_null() {
        return false;
    } else {
        _NtWaitForKeyedEvent.store(wait as usize, Ordering::Relaxed);
    }

    let notify = GetProcAddress(dll, b"NtReleaseKeyedEvent\0".as_ptr() as LPCSTR);
    if notify.is_null() {
        return false;
    } else {
        _NtReleaseKeyedEvent.store(notify as usize, Ordering::Relaxed);
    }

    let create = GetProcAddress(dll, b"NtCreateKeyedEvent\0".as_ptr() as LPCSTR);
    if create.is_null() {
        return false;
    }

    // racy creation of event handle as its faster than using a critical section.
    let mut handle = MaybeUninit::uninit();
    let NtCreateKeyedEvent: extern "stdcall" fn(
        EventHandle: PHANDLE,
        DesiredAccess: ACCESS_MASK,
        ObjectAttributes: PVOID,
        Flags: ULONG,
    ) -> NTSTATUS = transmute(create);
    if NtCreateKeyedEvent(
        handle.as_mut_ptr(),
        GENERIC_READ | GENERIC_WRITE,
        null_mut(),
        0,
    ) != STATUS_SUCCESS
    {
        return false;
    }

    // the handle to be stored first invalidates all other handles.
    // Use Release ordering so other threads see the loaded functions above.
    let handle = handle.assume_init();
    BACKEND_HANDLE
        .compare_exchange(0, handle as usize, Ordering::Release, Ordering::Relaxed)
        .map(|_| true)
        .unwrap_or_else(|_| {
            let is_handle_closed = CloseHandle(handle);
            debug_assert_eq!(is_handle_closed, TRUE, "Keyed Event handle leaked");
            true
        })
}
