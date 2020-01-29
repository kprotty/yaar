use super::WaitNode;
use futures_core::future::FusedFuture;
use core::{
    pin::Pin,
    future::Future,
    cell::UnsafeCell,
    task::{Poll, Context, Waker},
};

#[cfg(feature = "os")]
#[cfg_attr(feature = "nightly", doc(cfg(feature = "os")))]
pub type Mutex<T> = RawMutex<T, crate::sync::WordLock<crate::OsThreadParker>>;

pub struct RawMutex<T, Raw: lock_api::RawMutex> {
    state: lock_api::Mutex<Raw, usize>, 
    value: UnsafeCell<T>,
}

impl<T, Raw: lock_api::RawMutex> From<T> for RawMutex<T> {
    fn from(value: T) -> Self {
        Self::new(value)
    }
}

impl<T, Raw: lock_api::RawMutex> Default for RawMutex<T> {
    fn default() -> Self {
        Self::new(T::default())
    }
}

impl<T, Raw: lock_api::RawMutex> RawMutex<T> {
    pub fn new(value: T) -> Self {
        Self {
            state: lock_api::Mutex::new(0),
            value: UnsafeCell::new(value),
        }
    }

    pub fn into_inner(self) -> T {
        self.value.into_inner()
    }

    pub fn get_mut(&mut self) -> &mut T {
        unsafe { &mut *self.value.get() }
    }

    pub fn try_lock(&self) -> Option<RawMutexGuard<'_, T, Raw>> {
        self.state.try_lock().map(|mut state| {
            if *state & 1 == 0 {
                *state |= 1;
                Some(RawMutexGuard { mutex: self })
            } else {
                None
            }
        })
    }

    pub fn lock(&self) -> RawMutexLockFuture<'_, T, Raw> {
        RawMutexLockFuture {
            mutex: Some(self),
            wait_node: WaitNode::default(),
        }
    }
}

#[cfg(feature = "os")]
#[cfg_attr(feature = "nightly", doc(cfg(feature = "os")))]
pub type MutexLockFuture<'a, T> = RawMutexLockFuture<'a, T, crate::sync::WordLock<crate::OsThreadParker>>;

pub struct RawMutexLockFuture<'a, T, Raw: lock_api::RawMutex> {
    mutex: Option<&'a RawMutex<T, Raw>>,
    wait_node: WaitNode,
}

impl<'a, T, Raw: lock_api::RawMutex> FusedFuture for RawMutexLockFuture<'a, T, Raw> {
    fn is_terminated(&self) -> bool {
        self.mutex.is_none() 
    }
}

impl<'a, T, Raw: lock_api::RawMutex> Drop for RawMutexLockFuture<'a, T, Raw> {
    fn drop(&mut self) {
        if let Some(mutex) = self.mutex.take() {
            let mut state = mutex.state.lock();
            let head = (*state & !1) as *const WaitNode;
            let new_head = self.wait_node.remove(head);
            *state = (new_head as usize) | (*state & 1);
        }
    }
}

impl<'a, T, Raw: lock_api::RawMutex> Future for RawMutexLockFuture<'a, T, Raw> {
    type Output = RawMutexGuard<'a, T, Raw>;

    fn poll(self: Pin<&mut Self>, ctx: &mut Context<'_>) -> Poll<Self::Output> {
        // TODO: 
        Poll::Pending
    }
}

#[cfg(feature = "os")]
#[cfg_attr(feature = "nightly", doc(cfg(feature = "os")))]
pub type MutexGuard<'a, T> = RawMutexGuard<'a, T, crate::sync::WordLock<crate::OsThreadParker>>;

pub struct RawMutexGuard<'a, T, Raw: lock_api::RawMutex> {
    mutex: &'a RawMutex<T, Raw>,
}

