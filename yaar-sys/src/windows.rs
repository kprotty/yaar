
pub type PVOID = usize;
pub type HANDLE = PVOID;
pub type HMODULE = HANDLE;
pub const INVALID_HANDLE_VALUE: HANDLE = !0;

pub type BOOL = i32;
pub const TRUE: BOOL = 1;
pub const FALSE: BOOL = 0;

pub type BYTE = u8;
pub type WORD = u16;
pub type DWORD = u32;
pub type ULONG = DWORD;
pub type SIZE_T = usize;
pub type LARGE_INTEGER = i64;

pub type NTSTATUS = DWORD;
pub const STATUS_SUCCESS: NTSTATUS = 0;
pub const STATUS_TIMEOUT: NTSTATUS = 0x00000102;
pub const ERROR_TIMEOUT: DWORD = STATUS_TIMEOUT;

pub const INFINITE: DWORD = !0;
pub const GENERIC_READ: DWORD = 0x80000000;
pub const GENERIC_WRITE: DWORD = 0x40000000;

#[link(name = "kernel32")]
extern "stdcall" {
    pub fn GetLastError() -> DWORD;
    pub fn CloseHandle(handle: HANDLE) -> BOOL;

    pub fn GetModuleHandleW(moduleName: *const WORD) -> HMODULE;
    pub fn GetProcAddress(module: HMODULE, procName: *const BYTE) -> HANDLE;

    pub fn QueryPerformanceCounter(lpPerformanceCounter: &mut LARGE_INTEGER) -> BOOL;
    pub fn QueryPerformanceFrequency(lpPerformanceFrequency: &mut LARGE_INTEGER) -> BOOL;
}

pub type NtCreateKeyedEventFn = extern "stdcall" fn(
    KeyHandle: &mut HANDLE,
    dwAccessMask: DWORD,
    lpObjectAttributes: PVOID,
    Flags: ULONG,
) -> NTSTATUS;

pub type NtWaitForKeyedEventFn = extern "stdcall" fn(
    KeyHandle: HANDLE,
    KeyValue: PVOID,
    Alertable: BOOL,
    Timeout: *const LARGE_INTEGER,
) -> NTSTATUS;

pub type NtReleaseKeyedEventFn = extern "stdcall" fn(
    KeyHandle: HANDLE,
    KeyValue: PVOID,
    Alertable: BOOL,
    Timeout: *const LARGE_INTEGER,
) -> NTSTATUS;

pub type WakeByAddressSingleFn = extern "stdcall" fn(
    Address: PVOID,
);

pub type WaitOnAddressFn = extern "stdcall" fn(
    Address: PVOID,
    CompareAddress: PVOID,
    AddressSize: SIZE_T,
    dwMilliseconds: DWORD,
) -> BOOL;
