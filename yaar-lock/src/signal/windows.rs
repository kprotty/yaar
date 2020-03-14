#![allow(non_snake_case)]
#![allow(non_upper_case_globals)]

use core::{
    fmt,
    ptr::null,
    mem::{size_of, transmute},
    sync::atomic::{Ordering, AtomicU32, AtomicUsize},
};

pub struct OsSignal {
    state: AtomicU32,
}

const EMPTY: u32 = 0;
const WAITING: u32 = 1;
const NOTIFIED: u32 = 2;

impl OsSignal {
    pub const fn new() -> Self {
        Self {
            state: AtomicU32::new(EMPTY),
        }
    }
}

impl Default for OsSignal {
    fn default() -> Self {
        OsSignal::new()
    }
}

unsafe impl Send for OsSignal {}
unsafe impl Sync for OsSignal {}

impl fmt::Debug for OsSignal {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("OsSignal")
            .field("state", &match self.state.load(Ordering::Relaxed) {
                EMPTY => "Empty",
                WAITING => "Waiting",
                NOTIFIED => "Notified",
                _ => unreachable!("OsSignal.debug() invalid state"),
            })
            .finish()
    }
}

unsafe impl super::Signal for OsSignal {
    /// Either stores a notification token or wakes up a blocked thread that was waiting for one.
    /// Ensures a `Ordering::Release` barrier on notification sent.
    fn notify(&self) {
        let mut state = self.state.load(Ordering::Relaxed);
        loop {
            match state {
                EMPTY => match self.state.compare_exchange_weak(
                    EMPTY,
                    NOTIFIED,
                    Ordering::Release,
                    Ordering::Relaxed,
                ) {
                    Err(e) => state = e,
                    Ok(_) => return,
                },
                WAITING => unsafe {
                    self.state.store(EMPTY, Ordering::Release);
                    let state_ptr = &self.state as *const _ as windows::PVOID;
                    match Backend::get() {
                        Backend::KeyedEvent(handle) => {
                            let notify = _NtReleaseKeyedEvent.load(Ordering::Relaxed);
                            let notify = transmute::<usize, NtInvokeKeyedEvent>(notify);
                            let status = notify(handle, state_ptr, windows::FALSE, null());
                            debug_assert_eq!(status, windows::STATUS_SUCCESS, "NtReleaseKeyedEvent failed");
                        },
                        Backend::WaitOnAddress => {
                            let notify = _WakeByAddressSingle.load(Ordering::Relaxed);
                            let notify = transmute::<usize, WakeByAddressSingle>(notify);
                            notify(state_ptr);
                        },
                    }
                },
                NOTIFIED => return,
                _ => unreachable!("OsSignal.notify() invalid state"),
            }
        }
    }

    /// Either consumes a notification token set by a previous notify() call or blocks the current thread until a notify() call wakes us up.
    /// Ensures an `Ordering::Acquire` barrier on notification received.
    fn wait(&self) {
        let mut state = self.state.load(Ordering::Acquire);
        loop {
            match state {
                EMPTY => match self.state.compare_exchange_weak(
                    EMPTY,
                    WAITING,
                    Ordering::Acquire,
                    Ordering::Acquire,
                ) {
                    Err(e) => state = e,
                    Ok(_) => unsafe {
                        let state_ptr = &self.state as *const _ as windows::PVOID;
                        match Backend::get() {
                            Backend::KeyedEvent(handle) => {
                                let wait = _NtWaitForKeyedEvent.load(Ordering::Relaxed);
                                let wait = transmute::<usize, NtInvokeKeyedEvent>(wait);
                                let status = wait(handle, state_ptr, windows::FALSE, null());
                                debug_assert_eq!(status, windows::STATUS_SUCCESS, "NtWaitForKeyedEvent failed");
                            },
                            Backend::WaitOnAddress => {
                                let wait = _WaitOnAddress.load(Ordering::Relaxed);
                                let wait = transmute::<usize, WaitOnAddress>(wait);
                                let cmp_ptr = &WAITING as *const _ as windows::PVOID;
                                while self.state.load(Ordering::Acquire) != WAITING {
                                    let result = wait(state_ptr, cmp_ptr, size_of::<u32>(), windows::INFINITE);
                                    debug_assert_eq!(result, windows::TRUE as windows::BOOL, "WaitOnAddress failed");
                                }
                            }
                        }
                    },
                },
                WAITING => unreachable!("OsSignal only allows one waiting thread"),
                NOTIFIED => return self.state.store(EMPTY, Ordering::Relaxed),
                _ => unreachable!("OsSignal.wait() invalid state"),
            }
        }
    }
}

enum Backend {
    WaitOnAddress,
    KeyedEvent(windows::HANDLE),
}

impl Backend {
    pub const WAIT_ON_ADDRESS: usize = !0;

    /// Get a reference to the global backend handle which is used to identify the Signal OS backend
    pub fn handle() -> &'static AtomicUsize {
        static BACKEND_HANDLE: AtomicUsize = AtomicUsize::new(0);
        &BACKEND_HANDLE
    }

    /// Get, lazily/globally load, the Signal OS backend
    pub fn get() -> Self {
        match Self::handle().load(Ordering::Acquire) {
            0 => unsafe {
                if load_wait_on_address() {
                    Backend::WaitOnAddress
                } else if let Some(handle) = load_keyed_events() {
                    Backend::KeyedEvent(handle)
                } else {
                    unreachable!("OsSignal requires either WaitOnAddress (Win8+) or NT Keyed Events (WinXP+)")
                }
            },
            Self::WAIT_ON_ADDRESS => Backend::WaitOnAddress,
            handle => Backend::KeyedEvent(handle as windows::HANDLE),
        }
    }
}

static _WakeByAddressSingle: AtomicUsize = AtomicUsize::new(0);
type WakeByAddressSingle = extern "stdcall" fn(
    Address: windows::PVOID,
);

static _WaitOnAddress: AtomicUsize = AtomicUsize::new(0);
type WaitOnAddress = extern "stdcall" fn(
    Address: windows::PVOID,
    CompareAddress: windows::PVOID,
    AddressSize: windows::SIZE_T,
    dwMilliseconds: windows::DWORD,
) -> windows::BOOL;

unsafe fn load_wait_on_address() -> bool {
    // api-ms-win-core-synch-l1-2-0.dll
    let dll = windows::GetModuleHandleW(
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
        ]).as_ptr()
    );
    if dll.is_null() {
        return false;
    }

    let wait = windows::GetProcAddress(dll, b"WaitOnAddress\0".as_ptr());
    if wait.is_null() {
        return false;
    } else {
        _WaitOnAddress.store(wait as usize, Ordering::Relaxed);
    }

    let notify = windows::GetProcAddress(dll, b"WakeByAddressSingle\0".as_ptr());
    if notify.is_null() {
        return false;
    } else {
        _WakeByAddressSingle.store(notify as usize, Ordering::Relaxed);
    }

    // Use Release ordering so other threads see the loaded functions above.
    assert_eq!(Backend::WAIT_ON_ADDRESS, windows::INVALID_HANDLE_VALUE as usize);
    Backend::handle().store(Backend::WAIT_ON_ADDRESS, Ordering::Release);
    true
}

static _NtReleaseKeyedEvent: AtomicUsize = AtomicUsize::new(0);
static _NtWaitForKeyedEvent: AtomicUsize = AtomicUsize::new(0);
type NtInvokeKeyedEvent = extern "stdcall" fn(
    EventHandle: windows::HANDLE,
    Key: windows::PVOID,
    Alertable: windows::BOOLEAN,
    Timeout: *const windows::LARGE_INTEGER,
) -> windows::NTSTATUS;

unsafe fn load_keyed_events() -> Option<windows::HANDLE> {
    // ntdll.dll
    let dll = windows::GetModuleHandleW(
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
        ]).as_ptr(),
    );
    if dll.is_null() {
        return None;
    }

    let wait = windows::GetProcAddress(dll, b"NtWaitForKeyedEvent\0".as_ptr());
    if wait.is_null() {
        return None;
    } else {
        _NtWaitForKeyedEvent.store(wait as usize, Ordering::Relaxed);
    }

    let notify = windows::GetProcAddress(dll, b"NtReleaseKeyedEvent\0".as_ptr());
    if notify.is_null() {
        return None;
    } else {
        _NtReleaseKeyedEvent.store(notify as usize, Ordering::Relaxed);
    }

    let create = windows::GetProcAddress(dll, b"NtCreateKeyedEvent\0".as_ptr());
    if create.is_null() {
        return None;
    }

    // racy creation of event handle as its faster than using a critical section.
    let mut handle = windows::INVALID_HANDLE_VALUE;
    let NtCreateKeyedEvent: extern "stdcall" fn(
        EventHandle: *mut windows::HANDLE,
        DesiredAccess: windows::ACCESS_MASK,
        ObjectAttributes: windows::PVOID,
        Flags: windows::ULONG,
    ) -> windows::NTSTATUS = transmute(create);
    if NtCreateKeyedEvent(
        &mut handle,
        windows::GENERIC_READ | windows::GENERIC_WRITE,
        null(),
        0 as windows::ULONG,
    ) != windows::STATUS_SUCCESS
    {
        return None;
    }

    // The handle to be stored first invalidates all other handles.
    // Use Release ordering so other threads see the loaded functions above.
    Some(Backend::handle()
        .compare_exchange(0, handle as usize, Ordering::AcqRel, Ordering::Acquire)
        .map(|handle| handle as windows::HANDLE)
        .unwrap_or_else(|new_handle| {
            let is_handle_closed = windows::CloseHandle(handle);
            debug_assert_eq!(is_handle_closed, windows::TRUE as windows::BOOL, "Keyed Event handle leaked");
            new_handle as windows::HANDLE
        })
    )
}

mod windows {
    #![allow(non_camel_case_types)]

    pub type PVOID = *const u8;
    pub type HANDLE = PVOID;
    pub type FARPROC = PVOID;
    pub type HMODULE = HANDLE;
    pub const INVALID_HANDLE_VALUE: HANDLE = !0usize as HANDLE;

    pub type ULONG = u32;
    pub type DWORD = u32;
    pub type SIZE_T = usize;
    pub type LARGE_INTEGER = i64;
    pub const INFINITE: DWORD = !0;

    pub type NTSTATUS = DWORD;
    pub const STATUS_SUCCESS: NTSTATUS = 0;
    
    pub type ACCESS_MASK = DWORD;
    pub const GENERIC_READ: ACCESS_MASK = 0x80000000;
    pub const GENERIC_WRITE: ACCESS_MASK = 0x40000000;

    pub type BYTE = u8;
    pub type WORD = u16;
    pub type LPCSTR = *const BYTE;
    pub type LPCWSTR = *const WORD;
    
    pub type c_int = i32;
    pub type BOOL = c_int;
    pub type BOOLEAN = BYTE;
    pub const TRUE: BOOLEAN = 1;
    pub const FALSE: BOOLEAN = 0;

    #[link(name = "kernel32")]
    extern "stdcall" {
        pub fn CloseHandle(handle: HANDLE) -> BOOL;
        pub fn GetModuleHandleW(lpModuleName: LPCWSTR) -> HMODULE;
        pub fn GetProcAddress(hModule: HMODULE, lpProcName: LPCSTR) -> FARPROC;
    }
}