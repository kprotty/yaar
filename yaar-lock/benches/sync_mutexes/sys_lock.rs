use std::cell::UnsafeCell;

pub const NAME: &'static str = sys::Lock::NAME;

unsafe impl<T: Send> Send for Mutex<T> {}
unsafe impl<T: Send> Sync for Mutex<T> {}

pub struct Mutex<T> {
    sys_lock: sys::Lock,
    value: UnsafeCell<T>,
}

impl<T> Mutex<T> {
    pub fn new(value: T) -> Self {
        Self {
            sys_lock: sys::Lock::new(),
            value: UnsafeCell::new(value),
        }
    }

    pub fn lock<R>(&self, f: impl FnOnce(&mut T) -> R) -> R {
        self.sys_lock.acquire();
        let result = f(unsafe { &mut *self.value.get() });
        self.sys_lock.release();
        result
    }
}

#[cfg(windows)]
mod sys {
    use std::cell::UnsafeCell;

    pub struct Lock(UnsafeCell<SRWLOCK>);

    impl Lock {
        pub const NAME: &'static str = "SRWLOCK";

        pub fn new() -> Self {
            Self(UnsafeCell::new(SRWLOCK_INIT))
        }

        pub fn acquire(&self) {
            unsafe { AcquireSRWLockExclusive(self.0.get()) };
        }

        pub fn release(&self) {
            unsafe { ReleaseSRWLockExclusive(self.0.get()) };
        }
    }

    type SRWLOCK = usize;
    const SRWLOCK_INIT: SRWLOCK = 0;

    #[link(name = "kernel32")]
    extern "stdcall" {
        fn AcquireSRWLockExclusive(srwlock: *mut SRWLOCK);
        fn ReleaseSRWLockExclusive(srwlock: *mut SRWLOCK);
    }
}
