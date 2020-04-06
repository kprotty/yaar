
pub trait Mutex<T> {
    const NAME: &'static str;

    fn new(value: T) -> Self;

    fn locked<R>(&self, f: impl FnOnce(&mut T) -> R) -> R;
}

impl<T> Mutex<T> for std::sync::Mutex<T> {
    const NAME: &'static str = "std::sync::Mutex";

    fn new(value: T) -> Self {
        Self::new(value)
    }

    fn locked<R>(&self, f: impl FnOnce(&mut T) -> R) -> R {
        f(&mut *self.lock().unwrap())
    } 
}

impl<T> Mutex<T> for parking_lot::Mutex<T> {
    const NAME: &'static str = "parking_lot::Mutex";

    fn new(value: T) -> Self {
        Self::new(value)
    }

    fn locked<R>(&self, f: impl FnOnce(&mut T) -> R) -> R {
        f(&mut *self.lock())
    } 
}

impl<T> Mutex<T> for yaar_lock::sync::Mutex<T> {
    const NAME: &'static str = "yaar_lock::sync::Mutex";

    fn new(value: T) -> Self {
        Self::new(value)
    }

    fn locked<R>(&self, f: impl FnOnce(&mut T) -> R) -> R {
        f(&mut *self.lock())
    } 
}

pub mod std_lock;
impl<T> Mutex<T> for std_lock::Mutex<T> {
    const NAME: &'static str = "std_lock";

    fn new(value: T) -> Self {
        Self::new(value)
    }

    fn locked<R>(&self, f: impl FnOnce(&mut T) -> R) -> R {
        f(&mut *self.lock())
    } 
}

pub mod spin_lock;
impl<T> Mutex<T> for spin_lock::Mutex<T> {
    const NAME: &'static str = "spin_lock";

    fn new(value: T) -> Self {
        Self::new(value)
    }

    fn locked<R>(&self, f: impl FnOnce(&mut T) -> R) -> R {
        self.lock(f)
    } 
}

#[cfg(windows)]
pub mod nt_lock;
#[cfg(windows)]
impl<T> Mutex<T> for nt_lock::Mutex<T> {
    const NAME: &'static str = "NtKeyedEvents";

    fn new(value: T) -> Self {
        Self::new(value)
    }

    fn locked<R>(&self, f: impl FnOnce(&mut T) -> R) -> R {
        self.lock(f)
    } 
}