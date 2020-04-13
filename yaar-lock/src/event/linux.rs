use super::OsInstant;
use core::{
    fmt,
    ptr::null,
    sync::atomic::{AtomicUsize, Ordering},
};
use yaar_sys::{
    c_int, syscall, time_t, timespec, SYS_futex, EAGAIN, EINTR, ETIMEDOUT, FUTEX_PRIVATE_FLAG,
    FUTEX_WAIT, FUTEX_WAKE,
};

const EMPTY: usize = 0;
const WAITING: usize = 1;
const NOTIFIED: usize = 2;

pub struct Signal {
    state: AtomicUsize,
}

impl fmt::Debug for Signal {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let state = match self.state.load(Ordering::SeqCst) {
            EMPTY => "empty",
            WAITING => "has_waiter",
            NOTIFIED => "notified",
            _ => unreachable!(),
        };

        f.debug_struct("OsSignal").field("state", &state).finish()
    }
}

impl Signal {
    pub const fn new() -> Self {
        Self {
            state: AtomicUsize::new(EMPTY),
        }
    }

    pub fn notify(&self) {
        let mut state = self.state.load(Ordering::Relaxed);
        if state == EMPTY {
            state = self
                .state
                .compare_and_swap(EMPTY, NOTIFIED, Ordering::Release);
            if state == EMPTY {
                return;
            }
        }

        if state == WAITING {
            self.state.store(EMPTY, Ordering::Release);
            unsafe { futex_wake(&self.state) };
        }
    }

    pub fn try_wait(&self, timeout: Option<&OsInstant>) -> bool {
        let mut state = self.state.load(Ordering::Acquire);
        if state == EMPTY {
            state = self
                .state
                .compare_and_swap(EMPTY, WAITING, Ordering::Acquire);
            if state == EMPTY {
                let result = unsafe { futex_wait(&self.state, WAITING, EMPTY, timeout) };
                return result;
            }
        }

        assert_ne!(state, WAITING, "Multiple threads waiting on same OsSignal");
        assert_eq!(state, NOTIFIED, "Invalid OsSignal state");
        self.state.store(EMPTY, Ordering::Relaxed);
        true
    }
}

unsafe fn errno() -> c_int {
    #[cfg(target_os = "linux")]
    {
        *yaar_sys::__errno_location()
    }
    #[cfg(target_os = "android")]
    {
        *yaar_sys::__errno()
    }
}

unsafe fn futex_wake(ptr: &AtomicUsize) {
    let status = syscall(
        SYS_futex,
        ptr as *const _ as *const i32,
        FUTEX_WAKE | FUTEX_PRIVATE_FLAG,
        1,
    );
    debug_assert!(status == 0 || status == 1);
}

unsafe fn futex_wait(
    ptr: &AtomicUsize,
    expect: usize,
    reset: usize,
    timeout: Option<&OsInstant>,
) -> bool {
    // x32 Linux uses a non-standard type for tv_nsec in timespec.
    // See https://sourceware.org/bugzilla/show_bug.cgi?id=16437
    #[cfg(all(target_arch = "x86_64", target_pointer_width = "32"))]
    #[allow(non_camel_case_types)]
    type tv_nsec_t = i64;
    #[cfg(not(all(target_arch = "x86_64", target_pointer_width = "32")))]
    #[allow(non_camel_case_types)]
    type tv_nsec_t = yaar_sys::c_long;

    while ptr.load(Ordering::Acquire) == expect {
        let timeout = if let Some(timeout) = timeout {
            let now = OsInstant::now();
            if *timeout <= now {
                let timed_out = ptr.load(Ordering::Acquire) == expect
                    && ptr.compare_and_swap(expect, reset, Ordering::Acquire) == expect;
                return !timed_out;
            }

            let diff = *timeout - now;
            if diff.as_secs() as time_t as u64 != diff.as_secs() {
                None
            } else {
                Some(timespec {
                    tv_sec: diff.as_secs() as time_t,
                    tv_nsec: diff.subsec_nanos() as tv_nsec_t,
                })
            }
        } else {
            None
        };

        let ts_ptr = timeout
            .as_ref()
            .map(|ts_ref| ts_ref as *const _)
            .unwrap_or(null());

        let status = syscall(
            SYS_futex,
            ptr as *const _ as *const i32,
            FUTEX_WAIT | FUTEX_PRIVATE_FLAG,
            expect,
            ts_ptr,
        );

        debug_assert!(status == 0 || status == -1);
        if status == -1 {
            debug_assert!(
                errno() == EINTR
                    || errno() == EAGAIN
                    || (timeout.is_some() && errno() == ETIMEDOUT)
            );
        }
    }

    true
}
