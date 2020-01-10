use super::Event;
use core::cell::{Cell, UnsafeCell};
use libc::{
    pthread_cond_t,
    pthread_cond_wait,
    pthread_cond_signal,
    pthread_cond_destroy,
    PTHREAD_COND_INITIALIZER,
    pthread_mutex_t,
    pthread_mutex_lock,
    pthread_mutex_unlock,
    pthread_mutex_destroy,
    PTHREAD_MUTEX_INITIALIZER,
};

pub struct OsEvent {
    is_set: Cell<bool>,
    cond: UnsafeCell<pthread_cond_t>,
    mutex: UnsafeCell<pthread_mutex_t>,
}

impl Default for OsEvent {
    fn default() -> Self {
        Self::new()
    }
}

impl OsEvent {
    pub const fn new() -> Self {
        Self {
            is_set: Cell::new(false),
            cond: UnsafeCell::new(PTHREAD_COND_INITIALIZER),
            mutex: UnsafeCell::new(PTHREAD_MUTEX_INITIALIZER),
        }
    }
}

impl Drop for OsEvent {
    fn drop(&mut self) {
        // Seems as though the destroy functions can return EAGAIN
        // when called using statically initialized types on DragonflyBSD.
        unsafe {
            let r = pthread_mutex_destroy(self.mutex.get());
            if cfg!(target_os = "dragonfly") {
                debug_assert!(r == 0 || r == libc::EAGAIN);
            } else {
                debug_assert_eq!(r, 0);
            }

            let r = pthread_cond_destroy(self.cond.get());
            if cfg!(target_os = "dragonfly") {
                debug_assert!(r == 0 || r == libc::EAGAIN);
            } else {
                debug_assert_eq!(r, 0);
            }
        }
    }
}

unsafe impl Send for OsEvent {}
unsafe impl Sync for OsEvent {}

unsafe impl Event for OsEvent {
    fn reset(&mut self) {
        self.is_set.set(false);
    }

    fn set(&self) {
        unsafe {
            let r = pthread_mutex_lock(self.mutex.get());
            debug_assert_eq!(r, 0);

            if !self.is_set.get() {
                self.is_set.set(true);
                let r = pthread_cond_signal(self.cond.get());
                debug_assert_eq!(r, 0);
            }

            let r = pthread_mutex_unlock(self.mutex.get());
            debug_assert_eq!(r, 0);
        }
    }

    fn wait(&self) {
        unsafe {
            let r = pthread_mutex_lock(self.mutex.get());
            debug_assert_eq!(r, 0);

            while !self.is_set.get() {
                let r = pthread_cond_wait(self.cond.get(), self.mutex.get());
                debug_assert_eq!(r, 0);
            }

            let r = pthread_mutex_unlock(self.mutex.get());
            debug_assert_eq!(r, 0);
        }
    }
}
