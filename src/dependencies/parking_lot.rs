pub type MutexGuard<'a, T> = std::sync::MutexGuard<'a, T>;

pub struct Mutex<T> {
    inner: std::sync::Mutex<T>,
}

impl<T: Default> Default for Mutex<T> {
    fn default() -> Self {
        Self { inner: Default::default() }
    }
}

impl<T> Mutex<T> {
    pub fn new(value: T) -> Self {
        Self { inner: std::sync::Mutex::new(value) }
    }
    
    pub fn try_lock(&self) -> Option<MutexGuard<'_, T>> {
        self.inner.try_lock().ok()
    }

    pub fn lock(&self) -> MutexGuard<'_, T> {
        self.inner.lock().unwrap()
    }
}