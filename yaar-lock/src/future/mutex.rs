

pub struct GenericMutex<E, T> {

}

impl<E, T> GenericMutex<E, T> {
    pub const fn new(value: T) -> Self {
        Self {
            value: UnsafeCell::new(value),
            signal: FutureSignal::with_notification(),
        }
    }

    pub fn into_inner(self) -> T {
        self.value.into_inner()
    }

    pub fn get_mut(&mut self) -> &mut T {
        unsafe { &mut *self.value.get() }
    }

    #[inline]
    pub fn try_lock(&self) -> Option<GenericMutexGuard<'_, E, T>> {
        if self.signal.try_wait() {
            Some(GenericMutexGuard { mutex: self })
        } else {
            None
        }
    }

    pub async fn lock(&self) -> GenericMutexGuard<'_, E, T> {
        match self.signal.wait().await {
            RawSignalWaitResult::Notified => GenericMutexGuard { mutex: self },
            RawSignalWaitResult::Cancelled => unreachable!(),
        }
    }

    pub unsafe fn force_unlock(&self) {
        self.signal.notify_one();
    }
}