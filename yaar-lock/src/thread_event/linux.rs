use core::sync::atomic::{fence, AtomicI32, Ordering};
use libc::{
    syscall, SYS_futex, __errno_location, EAGAIN, EINTR, FUTEX_PRIVATE_FLAG, FUTEX_WAIT, FUTEX_WAKE,
};

const IS_RESET: i32 = 0;
const IS_WAITING: i32 = 1;
const IS_SET: i32 = 2;

pub struct Event {
    state: AtomicI32,
}

impl Default for Event {
    fn default() -> Self {
        Self {
            state: AtomicI32::new(IS_RESET),
        }
    }
}

impl Event {
    pub fn is_set(&self) -> bool {
        self.state.load(Ordering::Acquire) == IS_SET
    }

    pub fn reset(&self) {
        self.state.store(IS_RESET, Ordering::Relaxed);
    }

    pub fn set(&self) {
        // Check if theres a thread waiting to avoid an unnecessary FUTEX_WAKE if
        // possible.
        if self.state.swap(IS_SET, Ordering::Release) == IS_WAITING {
            let ptr = &self.state as *const _ as *const i32;
            let r = unsafe {
                syscall(
                    SYS_futex,
                    ptr,
                    FUTEX_WAKE | FUTEX_PRIVATE_FLAG,
                    i32::max_value(),
                )
            };
            debug_assert!(r >= 0);
        }
    }

    pub fn wait(&self) {
        // try to set the state to WAIT for the setter, exit if already set.
        let mut state = self.state.load(Ordering::Acquire);
        loop {
            if state == IS_SET {
                return;
            }
            match self.state.compare_exchange_weak(
                IS_RESET,
                IS_WAITING,
                Ordering::Relaxed,
                Ordering::Relaxed,
            ) {
                Ok(_) => break,
                Err(s) => {
                    fence(Ordering::Acquire);
                    state = s;
                }
            }
        }

        // wait until the state changes from WAIT to SET
        while self.state.load(Ordering::Acquire) != IS_SET {
            let ptr = &self.state as *const _ as *const i32;
            let r = unsafe {
                syscall(
                    SYS_futex,
                    ptr,
                    FUTEX_WAIT | FUTEX_PRIVATE_FLAG,
                    IS_WAITING,
                    0,
                )
            };
            debug_assert!(r == 0 || r == -1);
            if r == -1 {
                let errno = unsafe { *__errno_location() };
                debug_assert!(errno == EAGAIN || errno == EINTR);
            }
        }
    }
}
