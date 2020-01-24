use core::cell::{Cell, UnsafeCell};
use libc::{
    pthread_cond_destroy, pthread_cond_signal, pthread_cond_t, pthread_cond_wait,
    pthread_mutex_destroy, pthread_mutex_lock, pthread_mutex_t, pthread_mutex_unlock,
    PTHREAD_COND_INITIALIZER, PTHREAD_MUTEX_INITIALIZER,
};

pub struct Event {
    is_set: Cell<bool>,
    cond: UnsafeCell<pthread_cond_t>,
    mutex: UnsafeCell<pthread_mutex_t>,
}

impl Default for Event {
    fn default() -> Self {
        Self {
            is_set: Cell::new(false),
            cond: UnsafeCell::new(PTHREAD_COND_INITIALIZER),
            mutex: UnsafeCell::new(PTHREAD_MUTEX_INITIALIZER),
        }
    }
}

impl Drop for Event {
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

impl Event {
    pub fn reset(&self) {
        unsafe {
            let r = pthread_mutex_lock(self.mutex.get());
            debug_assert_eq!(r, 0);

            self.is_set.set(false);

            let r = pthread_mutex_unlock(self.mutex.get());
            debug_assert_eq!(r, 0);
        }
    }

    pub fn notify(&self) {
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

    pub fn wait(&self) {
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
