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
        let mut spin: usize = 0;
        let max_spin = if cfg!(windows) { 500 } else { 32 };
        loop {
            if !self.is_locked.load(Ordering::Relaxed) {
                if let Ok(_) = self.is_locked.compare_exchange_weak(
                    false,
                    true,
                    Ordering::Acquire,
                    Ordering::Relaxed,
                ) {
                    break;
                }
            }
            spin = spin.wrapping_add(1);
            (0..(1 << spin).min(max_spin)).for_each(|_| spin_loop_hint());
        }
        let result = f(unsafe { &mut *self.value.get() });
        self.is_locked.store(false, Ordering::Release);
        result
    }
}
