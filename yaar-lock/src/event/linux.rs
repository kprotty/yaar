use super::Event;
use core::sync::atomic::{AtomicI32, Ordering};
use libc::{
    syscall, SYS_futex, __errno_location, EAGAIN, EINTR, FUTEX_PRIVATE_FLAG, FUTEX_WAIT, FUTEX_WAKE,
};

const UNSET: i32 = 0;
const WAIT: i32 = 1;
const SET: i32 = 2;

/// The default [`Event`] implementation for linux.
/// Utilizes `futex()` for blocking and notification.
pub struct OsEvent {
    state: AtomicI32,
}

impl Default for OsEvent {
    fn default() -> Self {
        Self::new()
    }
}

impl OsEvent {
    pub const fn new() -> Self {
        Self {
            state: AtomicI32::new(UNSET),
        }
    }
}

unsafe impl Send for OsEvent {}
unsafe impl Sync for OsEvent {}

unsafe impl Event for OsEvent {
    fn reset(&mut self) {
        *self.state.get_mut() = UNSET;
    }

    fn set(&self) {
        // Check if theres a thread waiting to avoid an unnecessary FUTEX_WAKE if possible.
        if self.state.swap(SET, Ordering::Release) == WAIT {
            let ptr = &self.state as *const _ as *const i32;
            let r = unsafe { syscall(SYS_futex, ptr, FUTEX_WAKE | FUTEX_PRIVATE_FLAG, 1) };
            debug_assert!(r == 0 || r == 1);
        }
    }

    fn wait(&self) {
        // try to set the state to WAIT for the setter, exit if already set.
        let mut state = self.state.load(Ordering::Acquire);
        loop {
            if state == SET {
                return;
            }
            match self.state.compare_exchange_weak(
                UNSET,
                WAIT,
                Ordering::Acquire,
                Ordering::Acquire,
            ) {
                Err(s) => state = s,
                Ok(_) => break,
            }
        }

        // wait until the state changes from WAIT to SET
        while self.state.load(Ordering::Acquire) != SET {
            let ptr = &self.state as *const _ as *const i32;
            let r = unsafe { syscall(SYS_futex, ptr, FUTEX_WAIT | FUTEX_PRIVATE_FLAG, WAIT, 0) };
            debug_assert!(r == 0 || r == -1);
            if r == -1 {
                let errno = unsafe { *__errno_location() };
                debug_assert!(errno == EAGAIN || errno == EINTR);
            }
        }
    }
}
