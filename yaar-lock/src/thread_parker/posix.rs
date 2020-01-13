use super::ThreadParker;
use core::{
    cell::{Cell, UnsafeCell},
    task::Poll,
};
use libc::{
    pthread_cond_destroy, pthread_cond_signal, pthread_cond_t, pthread_cond_wait,
    pthread_mutex_destroy, pthread_mutex_lock, pthread_mutex_t, pthread_mutex_unlock,
    PTHREAD_COND_INITIALIZER, PTHREAD_MUTEX_INITIALIZER,
};

/// The default [`ThreadParker`] implementation for posix platforms that aren't linux.
/// Utilizes `pthread_cond_t/pthread_mutex_t` for parking and unparking.
pub struct Parker {
    is_set: Cell<bool>,
    cond: UnsafeCell<pthread_cond_t>,
    mutex: UnsafeCell<pthread_mutex_t>,
}

impl Default for Parker {
    fn default() -> Self {
        Self::new()
    }
}

impl Parker {
    pub const fn new() -> Self {
        Self {
            is_set: Cell::new(false),
            cond: UnsafeCell::new(PTHREAD_COND_INITIALIZER),
            mutex: UnsafeCell::new(PTHREAD_MUTEX_INITIALIZER),
        }
    }
}

impl Drop for Parker {
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

unsafe impl Sync for Parker {}

impl ThreadParker for Parker {
    type Context = ();

    fn from(_context: Self::Context) -> Self {
        Self::new()
    }

    fn reset(&self) {
        let r = pthread_mutex_lock(self.mutex.get());
        debug_assert_eq!(r, 0);

        self.is_set.set(false);

        let r = pthread_mutex_unlock(self.mutex.get());
        debug_assert_eq!(r, 0);
    }

    fn unpark(&self) {
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

    fn park(&self) -> Poll<()> {
        unsafe {
            let r = pthread_mutex_lock(self.mutex.get());
            debug_assert_eq!(r, 0);

            while !self.is_set.get() {
                let r = pthread_cond_wait(self.cond.get(), self.mutex.get());
                debug_assert_eq!(r, 0);
            }

            let r = pthread_mutex_unlock(self.mutex.get());
            debug_assert_eq!(r, 0);
            Poll::Ready(())
        }
    }
}
