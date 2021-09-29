use super::Once;
use std::{ffi::c_void, mem::MaybeUninit, cell::{Cell, UnsafeCell}, ptr};

pub struct ThreadLocal {
    once: Once,
    has_key: Cell<bool>,
    key: UnsafeCell<MaybeUninit<os::ThreadLocalKey>>,
}

unsafe impl Send for ThreadLocal {}
unsafe impl Sync for ThreadLocal {}

impl Default for ThreadLocal {
    fn default() -> Self {
        Self::new()
    }
}

impl Drop for ThreadLocal {
    fn drop(&mut self) {
        if self.has_key.get() {
            unsafe {
                let maybe_key = &mut *self.key.get();
                ptr::drop_in_place(maybe_key.as_mut_ptr());
            }
        }
    }
}

impl ThreadLocal {
    pub const fn new() -> Self {
        Self {
            once: Once::new(),
            has_key: Cell::new(false),
            key: UnsafeCell::new(MaybeUninit::uninit()),
        }
    }

    pub fn with<T>(&self, f: impl FnOnce(&mut *const c_void) -> T) -> T {
        unsafe {
            self.once.call_once(|| {
                let maybe_key = &mut *self.key.get();
                maybe_key.write(os::ThreadLocalKey::default());
                self.has_key.set(true);
            });

            let maybe_key = &*self.key.get();
            let key = &*maybe_key.as_ptr();

            let tls_key = key.get();
            let mut user_key = tls_key;

            let result = f(&mut user_key);
            if user_key != tls_key {
                key.set(user_key);
            }

            result
        }
    }
}

#[cfg(windows)]
mod os {
    use std::ffi::c_void;

    #[link(name = "kernel32")]
    extern "system" {
        fn TlsAlloc() -> u32;
        fn TlsFree(index: u32) -> i32;
        fn TlsGetValue(index: u32) -> *const c_void;
        fn TlsSetValue(index: u32, ptr: *const c_void) -> i32;
    }

    pub struct ThreadLocalKey {
        index: u32,
    }

    impl Default for ThreadLocalKey {
        fn default() -> Self {
            Self {
                index: unsafe {
                    let index = TlsAlloc();
                    assert_ne!(index, u32::MAX);
                    index
                }
            }
        }
    }

    impl Drop for ThreadLocalKey {
        fn drop(&mut self) {
            unsafe {
                let rc = TlsFree(self.index);
                assert_ne!(rc, 0);
            }
        }
    }

    impl ThreadLocalKey {
        pub fn get(&self) -> *const c_void {
            unsafe {
                TlsGetValue(self.index)
            }
        }

        pub fn set(&self, ptr: *const c_void) {
            unsafe {
                let rc = TlsSetValue(self.index, ptr);
                assert_ne!(rc, 0);
            }
        }
    }
}

#[cfg(unix)]
mod os {
    use std::{ffi::c_void, mem::MaybeUninit, cell::UnsafeCell};

    pub struct ThreadLocalKey {
        key: UnsafeCell<libc::pthread_key_t>,
    }

    unsafe impl Send for ThreadLocalKey {}
    unsafe impl Sync for ThreadLocalKey {}

    impl Default for ThreadLocalKey {
        fn default() -> Self {
            Self {
                key: UnsafeCell::new(unsafe {
                    let mut key = MaybeUninit::uninit();
                    let rc = libc::pthread_key_create(key.as_mut_ptr(), None);
                    assert_eq!(rc, 0);
                    key.assume_init()
                })
            }
        }
    }

    impl Drop for ThreadLocalKey {
        fn drop(&mut self) {
            unsafe {
                let key = *self.key.get();
                let rc = libc::pthread_key_delete(key);
                assert_eq!(rc, 0);
            }
        }
    }

    impl ThreadLocalKey {
        pub fn get(&self) -> *const c_void {
            unsafe {
                let key = *self.key.get();
                libc::pthread_getspecific(key)
            }
        }

        pub fn set(&self, ptr: *const c_void) {
            unsafe {
                let key = *self.key.get();
                let rc = libc::pthread_setspecific(key, ptr);
                assert_eq!(rc, 0);
            }
        }
    }
}