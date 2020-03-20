use core::{
    mem::transmute,
    num::NonZeroUsize,
    sync::atomic::{fence, AtomicU32, AtomicUsize, Ordering},
};

const EMPTY: u32 = 0;
const WAITING: u32 = 1;
const NOTIFIED: u32 = 2;

pub struct SystemSignal {
    state: AtomicU32,
}

impl SystemSignal {
    pub const fn new() -> Self {
        Self {
            state: AtomicU32::new(EMPTY),
        }
    }

    #[inline]
    pub fn notify(&self) {
        if self.state.swap(NOTIFIED, Ordering::Release) == WAITING {
            self.invoke_nt_event(|(_, notify)| (notify, "NtReleaseKeyedEvent"));
        }
    }

    #[inline]
    pub fn wait(&self) {
        if self.state.load(Ordering::Relaxed) == EMPTY {
            if self
                .state
                .compare_and_swap(EMPTY, WAITING, Ordering::Relaxed)
                == EMPTY
            {
                self.invoke_nt_event(|(wait, _)| (wait, "NtWaitForKeyedEvent"));
            }
        }
        
        debug_assert_eq!(self.state.load(Ordering::Relaxed), NOTIFIED);
        self.state.store(EMPTY, Ordering::Relaxed);
        fence(Ordering::Acquire);
    }

    #[cold]
    fn invoke_nt_event(
        &self,
        pick_fn: impl FnOnce(
            (&'static AtomicUsize, &'static AtomicUsize),
        ) -> (&'static AtomicUsize, &'static str),
    ) {
        #[link(name = "kernel32")]
        extern "stdcall" {
            fn CloseHandle(handle: usize) -> i32;
            fn GetModuleHandleW(moduleName: *const u16) -> usize;
            fn GetProcAddress(module: usize, procName: *const u8) -> usize;
        }

        const STATUS_SUCCESS: u32 = 0;
        const GENERIC_READ: u32 = 0x80000000;
        const GENERIC_WRITE: u32 = 0x40000000;
        const FALSE: u8 = 0;
        const TRUE: i32 = 1;

        unsafe {
            static NT_HANDLE: AtomicUsize = AtomicUsize::new(0);
            static NT_NOTIFY: AtomicUsize = AtomicUsize::new(0);
            static NT_WAIT: AtomicUsize = AtomicUsize::new(0);

            macro_rules! try_all {
                ($($body:tt)*) => ((|| Some({ $($body)* }))())
            };

            let handle = NonZeroUsize::new(NT_HANDLE.load(Ordering::Acquire))
                .map(|handle| handle.get())
                .or_else(|| {
                    try_all! {
                        let dll = NonZeroUsize::new(GetModuleHandleW(
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
                            ]).as_ptr()
                        ))?.get();

                        let notify = GetProcAddress(dll, b"NtReleaseKeyedEvent\0".as_ptr());
                        let notify = NonZeroUsize::new(notify)?.get();
                        NT_NOTIFY.store(notify, Ordering::Relaxed);

                        let wait = GetProcAddress(dll, b"NtWaitForKeyedEvent\0".as_ptr());
                        let wait = NonZeroUsize::new(wait)?.get();
                        NT_WAIT.store(wait, Ordering::Relaxed);

                        let create = GetProcAddress(dll, b"NtCreateKeyedEvent\0".as_ptr());
                        let create = NonZeroUsize::new(create)?.get();
                        let create: extern "stdcall" fn(
                            KeyHandle: &mut usize,
                            DesiredAccess: u32,
                            ObjectAttributes: usize,
                            Flags: u32,
                        ) -> u32 = transmute(create);

                        let mut handle = 0;
                        let status = create(&mut handle, GENERIC_READ | GENERIC_WRITE, 0, 0);
                        if status != STATUS_SUCCESS {
                            return None;
                        }

                        NT_HANDLE
                            .compare_exchange(0, handle, Ordering::AcqRel, Ordering::Acquire)
                            .map(|_| handle)
                            .unwrap_or_else(|new_handle| {
                                let status = CloseHandle(handle);
                                debug_assert_eq!(status, TRUE, "NtCreateKeyedEvent leaked handle");
                                new_handle
                            })
                    }
                })
                .expect("OsSignal on windows requires Nt Keyed Events (WinXP+)");

            let (invoke, invoke_fn) = pick_fn((&NT_WAIT, &NT_NOTIFY));
            let invoke = invoke.load(Ordering::Relaxed);
            let invoke: extern "stdcall" fn(
                KeyHandle: usize,
                Key: usize,
                Alertable: u8,
                TimeoutPtr: usize,
            ) -> u32 = transmute(invoke);

            let key = &self.state as *const _ as usize;
            let status = invoke(handle, key, FALSE, 0);
            debug_assert_eq!(status, STATUS_SUCCESS, "{} failed", invoke_fn);
        }
    }
}
