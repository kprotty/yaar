
#[cfg(target_os = "windows")]
mod os {
    use super::super::Instant;
    use std::{cell::{Cell, UnsafeCell}, ffi::c_void, ptr};

    #[link(name = "kernel32")]
    extern "system" {
        fn AcquireSRWLockExclusive(lock: *mut *mut c_void);
        fn ReleaseSRWLockExclusive(lock: *mut *mut c_void);
        fn WakeConditionVariable(cond: *mut *mut c_void);
        fn SleepConditionVariableSRW(
            cond: *mut *mut c_void,
            lock: *mut *mut c_void,
            millis: u32,
            flags: u32,
        ) -> i32;
    }

    pub struct Event {
        lock: UnsafeCell<*mut c_void>,
        cond: UnsafeCell<*mut c_void>,
        notified: Cell<bool>,
    }

    impl Event {
        pub const fn new() -> Self {
            Self {
                lock: UnsafeCell::new(ptr::null_mut()),
                cond: UnsafeCell::new(ptr::null_mut()),
                notified: Cell::new(false),
            }
        }

        fn locked<F>(&self, f: impl FnOnce() -> F) -> F {
            unsafe {
                AcquireSRWLockExclusive(self.lock.get());
                let result = f();
                ReleaseSRWLockExclusive(self.lock.get());
                result
            }
        }

        pub fn wait(&self, deadline: Option<Instant>) -> bool {
            self.locked(|| unsafe {
                loop {
                    if self.notified.get() {
                        return true;
                    }
                    
                    let mut timeout_ms = u32::MAX;
                    if let Some(deadline) = deadline {
                        if let Some(millis) = deadline.since_millis(Instant::now()) {
                            timeout_ms = millis.try_into().unwrap_or(u32::MAX).max(u32::MAX - 1);
                        } else {
                            return false;
                        }
                    }
                    
                    let _ = SleepConditionVariableSRW(
                        self.cond.get(),
                        self.lock.get(),
                        timeout_ms,
                        0u32,
                    );
                }
            });
        }

        pub fn notify(&self) {
            self.locked(|| unsafe {
                self.notified.set(true);
                WakeConditionVariable(self.cond.get());
            })
        }
    }
}

#[cfg(any(target_os = "linux", target_os = "android"))]
mod os {
    use super::super::Instant;
    use std::sync::atomic::{AtomicI32, Ordering};

    const EMPTY: i32 = 0;
    const WAITING: i32 = 1;
    const NOTIFIED: i32 = 2;

    pub struct Event {
        state: AtomicI32,    
    }

    impl Event {
        pub const fn new() -> Self {
            Self {
                state: AtomicI32::new(EMPTY),
            }
        }

        pub fn wait(&self, deadline: Option<Instant>) -> bool {
            match self.state.compare_exchange(
                EMPTY,
                WAITING,
                Ordering::Acquire,
                Ordering::Acquire,
            ) {
                Ok(_) => {},
                Err(NOTIFIED) => return true,
                Err(_) => unreachable!(),
            }

            loop {
                let mut ts_ptr = ptr::null::<libc::timespec>();
                let mut ts = libc::timespec {
                    tv_sec: 0,
                    tv_nsec: 0,
                };

                if let Some(deadline) = deadline {
                    match deadline.since_millis(Instant::now()) {
                        Some(millis) => {
                            ts_ptr = &ts;
                            ts.tv_sec = millis / 1_000;
                            ts.tv_nsec = (millis % 1_000) * 1_000_000;
                        },
                        None => {
                            return match self.state.compare_exchange(
                                WAITING,
                                EMPTY,
                                Ordering::Acquire,
                                Ordering::Acquire,
                            ) {
                                Ok(_) => false,
                                Err(NOTIFIED) => true,
                                _ => unreachable!(),
                            };
                        },
                    }
                }

                let _ = libc::syscall(
                    libc::SYS_futex,
                    &self.state as *const AtomicI32,
                    libc::FUTEX_PRIVATE_FLAG | libc::FUTEX_WAIT,
                    WAITING,
                    ts_ptr,
                );

                match self.state.load(Ordering::Acquire) {
                    WAITING => continue,
                    NOTIFIED => return true,
                    _ => unreachable!(),
                }
            }
        }

        pub fn notify(&self) {
            let state_ptr = self as *const AtomicI32;
            let result = self.state.swap(NOTIFIED, Ordering::Release);
            drop(self);

            match result {
                EMPTY => return,
                WAITING => {
                    let _ = libc::syscall(
                        libc::SYS_futex,
                        state_ptr,
                        libc::FUTEX_PRIVATE_FLAG | libc::FUTEX_WAKE,
                        1i32,
                    );
                },
                _ => unreachable!(),
            }
        }
    }
}

#[cfg(all(
    unix,
    not(target_os = "linux"),
    not(target_os = "android"),
))]
mod os {
    use super::super::Instant;

    pub struct Event {
        
    }

    impl Event {
        pub const fn new() -> Self {
            Self {
                
            }
        }

        pub fn wait(&self, deadline: Option<Instant>) -> bool {
            
        }

        pub fn notify(&self) {
            
        }
    }
}

