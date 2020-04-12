use std::cell::UnsafeCell;

pub const NAME: &'static str = os::Lock::NAME;

unsafe impl<T: Send> Send for Mutex<T> {}
unsafe impl<T: Send> Sync for Mutex<T> {}

pub struct Mutex<T> {
    os_lock: os::Lock,
    value: UnsafeCell<T>,
}

impl<T> Mutex<T> {
    pub fn new(value: T) -> Self {
        Self {
            os_lock: os::Lock::new(),
            value: UnsafeCell::new(value),
        }
    }

    pub fn lock<R>(&self, f: impl FnOnce(&mut T) -> R) -> R {
        self.os_lock.acquire();
        let result = f(unsafe { &mut *self.value.get() });
        self.os_lock.release();
        result
    }
}

#[cfg(windows)]
mod os {
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

#[cfg(unix)]
mod os {
    use std::cell::UnsafeCell;
    use yaar_sys::{
        EINVAL,
        pthread_mutex_t,
        PTHREAD_MUTEX_INITIALIZER,
        pthread_mutex_lock,
        pthread_mutex_unlock,
        pthread_mutex_destroy,
    };

    pub struct Lock(UnsafeCell<pthread_mutex_t>);

    impl Drop for Lock {
        fn drop(&mut self) {
            let status = unsafe { pthread_mutex_destroy(self.0.get()) };
            if cfg!(target_os = "dragonfly") {
                debug_assert!(status == 0 || status == EINVAL);
            } else {
                debug_assert_eq!(status, 0);
            }
        }
    }

    impl Lock {
        pub const NAME: &'static str = "pthread_mutex_t";

        pub fn new() -> Self {
            Self(UnsafeCell::new(PTHREAD_MUTEX_INITIALIZER))
        }

        pub fn acquire(&self) {
            let status = unsafe { pthread_mutex_lock(self.0.get()) };
            debug_assert_eq!(status, 0);
        }

        pub fn release(&self) {
            let status = unsafe { pthread_mutex_unlock(self.0.get()) };
            debug_assert_eq!(status, 0);
        }
    }
}