use std::{
    cell::UnsafeCell,
    sync::atomic::{spin_loop_hint, AtomicBool, Ordering},
};

pub struct Mutex<T> {
    is_locked: AtomicBool,
    value: UnsafeCell<T>,
}

unsafe impl<T: Send> Send for Mutex<T> {}
unsafe impl<T: Send> Sync for Mutex<T> {}

impl<T> Mutex<T> {
    pub fn new(value: T) -> Self {
        Self {
            is_locked: AtomicBool::new(false),
            value: UnsafeCell::new(value),
        }
    }

    pub fn lock<R>(&self, f: impl FnOnce(&mut T) -> R) -> R {
        self.acquire();
        let result = f(unsafe { &mut *self.value.get() });
        self.release();
        result
    }

    fn acquire(&self) {
        let mut spin: usize = 8;
        loop {
            if !self.is_locked.load(Ordering::Relaxed) {
                if let Ok(_) = self.is_locked.compare_exchange_weak(
                    false,
                    true,
                    Ordering::Acquire,
                    Ordering::Relaxed,
                ) {
                    return;
                }
            }
            if spin < 1024 {
                (0..spin).for_each(|_| spin_loop_hint());
                spin <<= 1;
            } else {
                (0..spin.min(10 * 1024)).for_each(|_| spin_loop_hint());
                spin = spin.wrapping_add(1024);
            }
        }
    }

    fn release(&self) {
        self.is_locked.store(false, Ordering::Release);
    }
}
