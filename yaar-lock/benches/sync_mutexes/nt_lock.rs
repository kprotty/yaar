use std::{
    mem::transmute,
    cell::UnsafeCell,
    sync::atomic::{spin_loop_hint, Ordering, AtomicU8, AtomicUsize},
};

const UNLOCKED: u8 = 0;
const LOCKED: u8 = 1;

const WAKE: usize = 1 << 8;
const WAIT: usize = 1 << 9;

pub struct Mutex<T> {
    state: AtomicUsize,
    value: UnsafeCell<T>,
}

unsafe impl<T: Send> Send for Mutex<T> {}
unsafe impl<T: Send> Sync for Mutex<T> {}

impl<T> Mutex<T> {
    pub fn new(value: T) -> Self {
        Self {
            state: AtomicUsize::new(UNLOCKED as usize),
            value: UnsafeCell::new(value),
        }
    }

    fn byte_state(&self) -> &AtomicU8 {
        unsafe { &*(&self.state as *const _ as *const _) }
    }

    pub fn lock<R>(&self, f: impl FnOnce(&mut T) -> R) -> R {
        self.acquire();
        let result = f(unsafe { &mut *self.value.get() });
        self.release();
        result
    }

    fn acquire(&self) {
        if self.byte_state().load(Ordering::Relaxed) == UNLOCKED {
            if let Ok(_) = self.byte_state().compare_exchange_weak(
                UNLOCKED,
                LOCKED,
                Ordering::Acquire,
                Ordering::Relaxed,
            ) {
                return;
            }
        }
        self.acquire_slow();
    }

    fn release(&self) {
        self.byte_state().store(UNLOCKED, Ordering::Release);
        self.release_slow();
    }

    #[cold]
    fn acquire_slow(&self) {
        let mut spin: usize = 0;
        let handle = Self::get_handle();
        let wait: NtInvoke = unsafe { transmute(NT_WAIT.load(Ordering::Relaxed)) };

        loop {
            spin_loop_hint();
            let waiters = self.state.load(Ordering::Relaxed);
            if waiters & (LOCKED as usize) == 0 {
                if let Ok(_) = self.byte_state().compare_exchange_weak(
                    UNLOCKED,
                    LOCKED,
                    Ordering::Acquire,
                    Ordering::Relaxed,
                ) {
                    return;
                }
                continue;
            }

            if (waiters & WAIT == 0) && spin < 2 {
                spin += 1;
                continue;
            }
            
            if let Ok(_) = self.state.compare_exchange_weak(
                waiters,
                waiters + WAIT,
                Ordering::Relaxed,
                Ordering::Relaxed,
            ) {
                let status = wait(handle, self as *const _ as usize, 0, 0);
                self.state.fetch_sub(WAKE, Ordering::Relaxed);
                debug_assert_eq!(status, 0, "NtWaitForKeyedEvent failed");
                spin = 0;
            }
        }
    }

    #[cold]
    fn release_slow(&self) {
        loop {
            spin_loop_hint();
            let waiters = self.state.load(Ordering::Relaxed);
            if (waiters < WAIT) || (waiters & (LOCKED as usize) != 0) || (waiters & WAKE != 0) {
                return;
            }
            if let Ok(_) = self.state.compare_exchange_weak(
                waiters,
                (waiters - WAIT) + WAKE,
                Ordering::Release,
                Ordering::Relaxed,
            ) {
                return unsafe {
                    let handle = Self::get_handle();
                    let notify: NtInvoke = transmute(NT_NOTIFY.load(Ordering::Relaxed));
                    let status = notify(handle, self as *const _ as usize, 0, 0);
                    debug_assert_eq!(status, 0, "NtReleaseKeyedEvent failed");
                };
            }
        }
    }

    fn get_handle() -> usize {
        let handle = NT_HANDLE.load(Ordering::Acquire);
        if handle != 0 {
            return handle;
        }
        Self::get_handle_slow()
    }

    #[cold]
    fn get_handle_slow() -> usize {
        unsafe {
            let dll = GetModuleHandleW((&[
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
            ]).as_ptr());
            assert_ne!(dll, 0, "Failed to load ntdll.dll");

            let notify = GetProcAddress(dll, b"NtReleaseKeyedEvent\0".as_ptr());
            assert_ne!(notify, 0, "Failed to load NtReleaseKeyedEvent");
            NT_NOTIFY.store(notify, Ordering::Relaxed);

            let wait = GetProcAddress(dll, b"NtWaitForKeyedEvent\0".as_ptr());
            assert_ne!(wait, 0, "Failed to load NtWaitForKeyedEvent");
            NT_WAIT.store(wait, Ordering::Relaxed);

            let create = GetProcAddress(dll, b"NtCreateKeyedEvent\0".as_ptr());
            assert_ne!(create, 0, "Failed to load NtCreateKeyedEvent");
            let create: NtCreate = transmute(create);

            let mut handle = 0;
            let status = create(&mut handle, 0x80000000 | 0x40000000, 0, 0);
            assert_eq!(status, 0, "Failed to create NT Keyed Event Handle");

            match NT_HANDLE.compare_exchange(
                0,
                handle,
                Ordering::Release,
                Ordering::Relaxed,
            ) {
                Ok(_) => handle,
                Err(new_handle) => {
                    let status = CloseHandle(handle);
                    assert_eq!(status, 1, "Failed to close extra NT Keyed Event Handle");
                    new_handle
                }
            }
        }
    }
}

static NT_WAIT: AtomicUsize = AtomicUsize::new(0);
static NT_NOTIFY: AtomicUsize = AtomicUsize::new(0);
static NT_HANDLE: AtomicUsize = AtomicUsize::new(0);

type NtCreate = extern "stdcall" fn(
    handle: &mut usize,
    mask: u32,
    unused: usize,
    unused: usize,
) -> u32;
type NtInvoke = extern "stdcall" fn(
    handle: usize,
    key: usize,
    block: usize,
    timeout: usize,
) -> u32;

#[link(name = "kernel32")]
extern "stdcall" {
    fn CloseHandle(handle: usize) -> i32;
    fn GetModuleHandleW(moduleName: *const u16) -> usize;
    fn GetProcAddress(module: usize, procName: *const u8) -> usize;
}
