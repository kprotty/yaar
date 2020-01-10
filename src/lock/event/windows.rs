#![allow(non_upper_case_globals, non_snake_case)]

use super::Event;
use core::{
    ptr::null_mut,
    mem::{size_of, transmute, MaybeUninit},
    sync::atomic::{Ordering, AtomicU32, AtomicUsize},
};
use winapi::{
    shared::{
        basetsd::SIZE_T,
        ntdef::{NTSTATUS, FALSE},
        ntstatus::STATUS_SUCCESS,
        minwindef::{BOOL, TRUE, DWORD, ULONG},
    },
    um::{
        winbase::INFINITE,
        handleapi::{CloseHandle, INVALID_HANDLE_VALUE},
        libloaderapi::{GetModuleHandleA, GetProcAddress},
        winnt::{PVOID, PHANDLE, HANDLE, PLARGE_INTEGER, ACCESS_MASK, BOOLEAN, GENERIC_READ, GENERIC_WRITE, LPCSTR},
    },
};

const UNSET: u32 = 0;
const WAIT: u32 = 1;
const SET: u32 = 2;

pub struct OsEvent {
    state: AtomicU32,
}

impl Default for OsEvent {
    fn default() -> Self {
        Self::new()
    }
}

impl OsEvent {
    pub const fn new() -> Self {
        Self {
            state: AtomicU32::new(UNSET)
        }
    }
}

unsafe impl Send for OsEvent {}
unsafe impl Sync for OsEvent {}

unsafe impl Event for OsEvent {
    fn reset(&mut self) {
        *self.state.get_mut() = UNSET;
    }

    fn set(&self) {
        if self.state.swap(SET, Ordering::Release) == WAIT {
            unsafe {
                match get_backend() {
                    Backend::KeyedEvent(handle) => {
                        let NtReleaseKeyedEvent: extern "stdcall" fn(
                            EventHandle: HANDLE,
                            Key: PVOID,
                            Alertable: BOOLEAN,
                            Timeout: PLARGE_INTEGER,
                        ) -> NTSTATUS = transmute(_NtReleaseKeyedEvent.load(Ordering::Relaxed));
                        let r = NtReleaseKeyedEvent(
                            handle,
                            &self.state as *const _ as PVOID,
                            FALSE,
                            null_mut(),
                        );
                        debug_assert_eq!(r, STATUS_SUCCESS);
                    },
                    Backend::WaitOnAddress => {
                        let WakeByAddressSingle: extern "stdcall" fn(
                            Address: PVOID,
                        ) = transmute(_WakeByAddressSingle.load(Ordering::Relaxed));
                        WakeByAddressSingle(&self.state as *const _ as PVOID);
                    },
                }
            }
        }
    }

    fn wait(&self) {
        let mut state = self.state.load(Ordering::Acquire);
        loop {
            if state == SET {
                return;
            }
            match self.state.compare_exchange_weak(UNSET, WAIT, Ordering::Acquire, Ordering::Acquire) {
                Err(s) => state = s,
                Ok(_) => break,
            }
        }

        unsafe {
            match get_backend() {
                Backend::KeyedEvent(handle) => {
                    let NtWaitForKeyedEvent: extern "stdcall" fn(
                        EventHandle: HANDLE,
                        Key: PVOID,
                        Alertable: BOOLEAN,
                        Timeout: PLARGE_INTEGER,
                    ) -> NTSTATUS = transmute(_NtWaitForKeyedEvent.load(Ordering::Relaxed));
                    let r = NtWaitForKeyedEvent(
                        handle,
                        &self.state as *const _ as PVOID,
                        FALSE,
                        null_mut(),
                    );
                    debug_assert_eq!(r, STATUS_SUCCESS);
                },
                Backend::WaitOnAddress => {
                    let WaitOnAddress: extern "stdcall" fn(
                        Address: PVOID,
                        CompareAddress: PVOID,
                        AddressSize: SIZE_T,
                        dwMilliseconds: DWORD,
                    ) -> BOOL = transmute(_WaitOnAddress.load(Ordering::Relaxed));
                    while self.state.load(Ordering::Acquire) != SET {
                        let r = WaitOnAddress(
                            &self.state as *const _ as PVOID,
                            &WAIT as *const _ as PVOID,
                            size_of::<u32>() as SIZE_T,
                            INFINITE,
                        );
                        debug_assert_eq!(r, TRUE);
                    }
                },
            }
        }
    }
}

enum Backend {
    WaitOnAddress,
    KeyedEvent(HANDLE),
}

const WAIT_ON_ADDRESS: usize = !0;
static BACKEND_HANDLE: AtomicUsize = AtomicUsize::new(0);

unsafe fn get_backend() -> Backend {
    match BACKEND_HANDLE.load(Ordering::Acquire) {
        0 => {
            if load_wait_on_address() {
                Backend::WaitOnAddress
            } else if load_keyed_events() {
                Backend::KeyedEvent(BACKEND_HANDLE.load(Ordering::Relaxed) as HANDLE)
            } else {
                unreachable!("OsEvent requires either WaitOnAddress (Win8+) or NT Keyed Events (WinXP+)")
            }
        },
        WAIT_ON_ADDRESS => Backend::WaitOnAddress,
        handle => Backend::KeyedEvent(handle as HANDLE),
    }
}

static _WaitOnAddress: AtomicUsize = AtomicUsize::new(0);
static _WakeByAddressSingle: AtomicUsize = AtomicUsize::new(0);

unsafe fn load_wait_on_address() -> bool {
    let dll = GetModuleHandleA(b"api-ms-win-core-synch-l1-2-0.dll\0".as_ptr() as LPCSTR);
    if dll.is_null() {
        return false;
    }

    let WaitOnAddress = GetProcAddress(dll, b"WaitOnAddress\0".as_ptr() as LPCSTR);
    if WaitOnAddress.is_null() {
        return false;
    } else {
        _WaitOnAddress.store(WaitOnAddress as usize, Ordering::Relaxed);
    }

    let WakeByAddressSingle = GetProcAddress(dll, b"WakeByAddressSingle\0".as_ptr() as LPCSTR);
    if WakeByAddressSingle.is_null() {
        return false;
    } else {
        _WakeByAddressSingle.store(WakeByAddressSingle as usize, Ordering::Relaxed);
    }

    assert_eq!(WAIT_ON_ADDRESS, INVALID_HANDLE_VALUE as usize);
    BACKEND_HANDLE.store(WAIT_ON_ADDRESS, Ordering::Release);
    true
}

static _NtReleaseKeyedEvent: AtomicUsize = AtomicUsize::new(0);
static _NtWaitForKeyedEvent: AtomicUsize = AtomicUsize::new(0);

unsafe fn load_keyed_events() -> bool {
    let dll = GetModuleHandleA(b"ntdll.dll\0".as_ptr() as LPCSTR);
    if dll.is_null() {
        return false;
    }

    let NtReleaseKeyedEvent = GetProcAddress(dll, b"NtReleaseKeyedEvent\0".as_ptr() as LPCSTR);
    if NtReleaseKeyedEvent.is_null() {
        return false;
    } else {
        _NtReleaseKeyedEvent.store(NtReleaseKeyedEvent as usize, Ordering::Relaxed);
    }

    let NtWaitForKeyedEvent = GetProcAddress(dll, b"NtWaitForKeyedEvent\0".as_ptr() as LPCSTR);
    if NtWaitForKeyedEvent.is_null() {
        return false;
    } else {
        _NtWaitForKeyedEvent.store(NtWaitForKeyedEvent as usize, Ordering::Relaxed);
    }

    let NtCreateKeyedEvent = GetProcAddress(dll, b"NtWaitForKeyedEvent\0".as_ptr() as LPCSTR);
    if NtCreateKeyedEvent.is_null() {
        return false;
    }

    let NtCreateKeyedEvent: extern "stdcall" fn(
        KeyedEventHandle: PHANDLE,
        DesiredAccess: ACCESS_MASK,
        ObjectAttributes: PVOID,
        Flags: ULONG,
    ) -> NTSTATUS = transmute(NtCreateKeyedEvent);
    let mut handle = MaybeUninit::uninit();
    let r = NtCreateKeyedEvent(
        handle.as_mut_ptr(),
        GENERIC_READ | GENERIC_WRITE,
        null_mut(),
        0 as ULONG,
    );
    if r != STATUS_SUCCESS {
        return false;
    }

    let handle = handle.assume_init();
    match BACKEND_HANDLE.compare_exchange(0, handle as usize, Ordering::Release, Ordering::Relaxed) {
        Ok(_) => true,
        Err(_) => {
            let r = CloseHandle(handle);
            debug_assert_eq!(r, TRUE);
            true
        }
    }
}
