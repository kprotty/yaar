use std::{
    convert::TryInto,
    sync::atomic::{AtomicIsize, Ordering},
};

#[derive(Default)]
pub struct IdleQueue {
    value: AtomicIsize,
    semaphore: os::Semaphore,
}

impl IdleQueue {
    pub fn try_wait(&self) -> bool {
        self.value
            .fetch_update(Ordering::Acquire, Ordering::Relaxed, |value| match value {
                v if v > 0 => Some(v - 1),
                _ => None,
            })
            .is_ok()
    }

    pub fn wait(&self) {
        let value = self.value.fetch_sub(1, Ordering::Acquire);
        assert_ne!(value, isize::MIN);

        if value <= 0 {
            self.semaphore.wait();
        }
    }

    pub fn post(&self, count: u16) -> bool {
        let count = count as isize;
        let value = self.value.fetch_add(count, Ordering::Release);
        assert!(value.checked_add(count).is_some());

        if value >= 0 {
            return false;
        }

        let waiters: u64 = (0 - value).min(count).try_into().unwrap();
        for _ in 0..waiters {
            self.semaphore.post();
        }

        true
    }
}

#[cfg(target_os = "windows")]
mod os {
    use std::{ffi::c_void, ptr};

    #[link(name = "kernel32")]
    extern "system" {
        fn CreateSemaphoreA(
            attr: *mut c_void,
            init_count: i32,
            max_count: i32,
            name: *const c_void,
        ) -> *mut c_void;
        fn CloseHandle(handle: *mut c_void) -> i32;
        fn WaitForSingleObject(handle: *mut c_void, millis: u32) -> u32;
        fn ReleaseSemaphore(handle: *mut c_void, count: i32, prev_count: *mut i32) -> i32;
    }

    pub struct Semaphore {
        sema: *mut c_void,
    }

    unsafe impl Send for Semaphore {}
    unsafe impl Sync for Semaphore {}

    impl Default for Semaphore {
        fn default() -> Self {
            unsafe {
                let sema = CreateSemaphoreA(ptr::null_mut(), 0, i32::MAX, ptr::null());
                assert!(!sema.is_null());
                Self { sema }
            }
        }
    }

    impl Drop for Semaphore {
        fn drop(&mut self) {
            let rc = unsafe { CloseHandle(self.sema) };
            assert_ne!(rc, 0);
        }
    }

    impl Semaphore {
        pub fn wait(&self) {
            match unsafe { WaitForSingleObject(self.sema, u32::MAX) } {
                0 => {}
                0x80 => unreachable!("mutex error code for semaphore"),
                0x102 => unreachable!("semaphore timed out with INFINITE"),
                0xffffffff => unreachable!("semaphore wait failed"),
                _ => unreachable!("invalid semaphore wait result"),
            }
        }

        pub fn post(&self) {
            let rc = unsafe { ReleaseSemaphore(self.sema, 1, ptr::null_mut()) };
            assert_ne!(rc, 0);
        }
    }
}

#[cfg(target_vendor = "apple")]
mod os {
    use std::{cell::UnsafeCell, ffi::c_void};

    #[link(name = "c")]
    extern "C" {
        fn dispatch_release(sema: *mut c_void);
        fn dispatch_semaphore_create(value: isize) -> *mut c_void;
        fn dispatch_semaphore_wait(sema: *mut c_void, time: u64) -> isize;
        fn dispatch_semaphore_signal(sema: *mut c_void) -> isize;
    }

    pub struct Semaphore {
        sema: *mut c_void,
    }

    unsafe impl Send for Semaphore {}
    unsafe impl Sync for Semaphore {}

    impl Default for Semaphore {
        fn default() -> Self {
            unsafe {
                let sema = dispatch_semaphore_create(0);
                assert!(!sema.is_null());
                Self { sema }
            }
        }
    }

    impl Drop for Semaphore {
        fn drop(&mut self) {
            unsafe { dispatch_release(self.sema) }
        }
    }

    impl Semaphore {
        pub fn wait(&self) {
            let rc = unsafe { dispatch_semaphore_wait(self.sema, u64::MAX) };
            assert_eq!(rc, 0);
        }

        pub fn post(&self) {
            let _ = unsafe { dispatch_semaphore_signal(self.sema) };
        }
    }
}

#[cfg(all(unix, not(target_vendor = "apple")))]
mod os {
    use std::{cell::UnsafeCell, mem::MaybeUninit};

    pub struct Semaphore {
        sem: UnsafeCell<libc::sem_t>,
    }

    unsafe impl Send for Semaphore {}
    unsafe impl Sync for Semaphore {}

    impl Default for Semaphore {
        fn default() -> Self {
            Self {
                sem: UnsafeCell::new(unsafe {
                    let mut sem = MaybeUninit::uninit();
                    let rc = libc::sem_init(sem.as_mut_ptr(), 0, 0);
                    assert_eq!(rc, 0);
                    sem.assume_init()
                }),
            }
        }
    }

    impl Drop for Semaphore {
        fn drop(&mut self) {
            let rc = unsafe { libc::sem_destroy(self.sem.get()) };
            assert_eq!(rc, 0);
        }
    }

    impl Semaphore {
        pub fn wait(&self) {
            let rc = unsafe { libc::sem_wait(self.sem.get()) };
            assert_eq!(rc, 0);
        }

        pub fn post(&self) {
            let rc = unsafe { libc::sem_post(self.sem.get()) };
            assert_eq!(rc, 0);
        }
    }
}
