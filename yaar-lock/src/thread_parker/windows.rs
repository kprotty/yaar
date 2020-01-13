#![allow(non_upper_case_globals, non_snake_case)]

use super::ThreadParker;
use core::{
    mem::{size_of, transmute, MaybeUninit},
    ptr::null_mut,
    sync::atomic::{AtomicU32, AtomicUsize, Ordering},
    task::Poll,
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
        libloaderapi::{GetModuleHandleA, GetProcAddress},
        winbase::INFINITE,
        winnt::{
            ACCESS_MASK, BOOLEAN, GENERIC_READ, GENERIC_WRITE, HANDLE, LPCSTR, PHANDLE,
            PLARGE_INTEGER, PVOID,
        },
    },
};

const UNSET: u32 = 0;
const WAIT: u32 = 1;
const SET: u32 = 2;

/// The default [`ThreadParker`] implementation for windows.
/// Relies on [`WaitOnAddress`], falling back to [`NT Keyed Events`],
/// for parking and unparking.
///
/// [`WaitOnAddress`]: https://docs.microsoft.com/en-us/windows/win32/api/synchapi/nf-synchapi-waitonaddress
/// [`NT Keyed Events`]: https://locklessinc.com/articles/keyed_events/
pub struct Parker {
    state: AtomicU32,
}

impl Default for Parker {
    fn default() -> Self {
        Self::new()
    }
}

impl Parker {
    pub const fn new() -> Self {
        Self {
            state: AtomicU32::new(UNSET),
        }
    }
}

unsafe impl Sync for Parker {}

impl ThreadParker for Parker {
    type Context = ();

    fn from(_context: Self::Context) -> Self {
        Self::new()
    }

    fn reset(&self) {
        self.state.store(UNSET, Ordering::Relaxed);
    }

    fn unpark(&self) {
        // Only do the wake up if theres a thread waiting.
        // Prevents a deadlock on NtReleaseKeyedEvent and an extra syscall for WakeByAddressSingle.
        if self.state.swap(SET, Ordering::Release) == WAIT {
            unsafe {
                match get_backend() {
                    Backend::KeyedEvent(handle) => {
                        let NtReleaseKeyedEvent: extern "stdcall" fn(
                            EventHandle: HANDLE,
                            Key: PVOID,
                            Alertable: BOOLEAN,
                            Timeout: PLARGE_INTEGER,
                        )
                            -> NTSTATUS = transmute(_NtReleaseKeyedEvent.load(Ordering::Relaxed));
                        let r = NtReleaseKeyedEvent(
                            handle,
                            &self.state as *const _ as PVOID,
                            FALSE,
                            null_mut(),
                        );
                        debug_assert_eq!(r, STATUS_SUCCESS);
                    }
                    Backend::WaitOnAddress => {
                        let WakeByAddressSingle: extern "stdcall" fn(Address: PVOID) =
                            transmute(_WakeByAddressSingle.load(Ordering::Relaxed));
                        WakeByAddressSingle(&self.state as *const _ as PVOID);
                    }
                }
            }
        }
    }

    fn park(&self) -> Poll<()> {
        // Try to transition in the waiting state.
        let mut state = self.state.load(Ordering::Acquire);
        loop {
            if state == SET {
                return Poll::Ready(());
            }
            match self.state.compare_exchange_weak(
                UNSET,
                WAIT,
                Ordering::Acquire,
                Ordering::Acquire,
            ) {
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
                }
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
                }
            }
        }

        Poll::Ready(())
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
                unreachable!(
                    "OsEvent requires either WaitOnAddress (Win8+) or NT Keyed Events (WinXP+)"
                )
            }
        }
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

    // Release the stores to the function pointers above for threads calling them to use.
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

    // Release the stores to the function pointers above for threads calling them to use.
    // Racy submission of a keyed event handle (appears faster than using a spinlock).
    let handle = handle.assume_init();
    match BACKEND_HANDLE.compare_exchange(0, handle as usize, Ordering::Release, Ordering::Relaxed)
    {
        Ok(_) => true,
        Err(_) => {
            let r = CloseHandle(handle);
            debug_assert_eq!(r, TRUE);
            true
        }
    }
}
