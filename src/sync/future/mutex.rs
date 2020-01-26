use crate::sync::{ThreadEvent, RawMutex as RawMutexSync};
use futures_core::future::{Future, FusedFuture};
use core::{
    pin::Pin,
    cell::UnsafeCell,
    task::{Poll, Context},
};

pub struct RawMutex<T, Event: ThreadEvent> {
    state: RawMutexSync<usize, Event>,
    value: UnsafeCell<T>,
}

impl<T: Default, Event: ThreadEvent> Default for RawMutex<T, Event> {
    fn default() -> Self {
        Self::new(T::default())
    }
}

impl<T, Event: ThreadEvent> RawMutex<T, Event> {
    pub fn new(value: T) -> Self {
        Self {
            state: RawMutexSync::new(0),
            value: UnsafeCell::new(value),
        }
    }

    pub fn into_inner(self) -> T {
        self.value.into_inner()
    }

    pub fn try_lock(&self) -> Option<RawMutexGuard<'_, T, Event>> {
        if let Some(mut state) = self.state.try_lock() {
            if state & 1 == 0 {
                *state |= 1;
                Some(RawMutexGuard{ mutex: self })
            } else {
                None
            }
        } else {
            None
        }
    }

    pub fn lock(&self) -> RawMutexLockFuture<'a, T, Event> {
        RawMutexLockFuture {
            mutex: self,
            wait_node: WaitNode::default(),
        }
    }
}

pub struct RawMutexLockFuture<'a, T, Event: ThreadEvent> {
    mutex: &'a RawMutex<T, Event>,
    wait_node: WaitNode,
}

impl<'a, T, Event: ThreadEvent> FusedFuture for RawMutexLockFuture<'a, T, Event> {
    fn is_terminated(&self) -> bool {
        wait_node.state.load(Ordering::Acquire) == NOTIFIED
    }
}

impl<'a, T, Event: ThreadEvent> Drop for RawMutexLockFuture<'a, T, Event> {
    fn drop(&mut self) {
        if !self.is_terminated() {
            let mut state = self.mutex.state.lock();
            self.wait_node.remove(&mut *state);
        } 
    }
}

impl<'a, T, Event: ThreadEvent> Future for RawMutexLockFuture<'a, T, Event> {
    type Output = RawMutexGuard<'a, T, Event>;

    fn poll(self: Pin<&mut Self>, ctx: &mut Context<'_>) -> Poll<Self::Output> {
        match self.wait_node.state.load(Ordering::Acquire) {
            NOTIFIED => Poll::Ready(RawMutexGuard { mutex: self.mutex })
            WAITING => Poll::Pending,
            UNINIT => {
                let mut state = self.mutex.state.lock();
                if state & 1 == 0 {
                    *state |= 1;
                    Poll::Ready(RawMutexGuard { mutex: self.mutex }) 
                } else {
                    let head = (state & !1) as *const WaitNode;
                    self.wait_node.next.set(head);
                    if head.is_null() {
                        self.wait_node.tail.set(&self.wait_node);
                    } else {
                        self.wait_node.tail.set()
                    }
                }
            },
        }
    }
}