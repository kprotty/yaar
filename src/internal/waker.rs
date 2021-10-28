use std::{
    cell::UnsafeCell,
    mem,
    sync::atomic::{AtomicUsize, Ordering},
    task::{Poll, Waker, RawWaker, RawWakerVtable},
};

#[derive(Copy, Clone, Eq, PartialEq, Debug)]
enum Status {
    Empty = 0,
    Updating = 1,
    Ready = 2,
    Waking = 3,
    Notified = 4,
}

#[derive(Copy, Clone)]
struct State {
    token: usize,
    status: Status,
}

impl Into<usize> for State {
    fn into(self) -> usize {
        (self.token << 3) | (self.status as usize)
    }
}

impl From<usize> for State {
    fn from(value: usize) -> Self {
        Self {
            token: value >> 3,
            status: match value & 0b111 {
                0 => Status::Empty,
                1 => Status::Updating,
                2 => Status::Ready,
                3 => Status::Waking,
                4 => Status::Notified,
                _ => unreachable!(),
            },
        }
    }
}

#[derive(Default)]
pub struct AtomicWaker {
    state: AtomicUsize,
    waker: UnsafeCell<Option<Waker>>,
}

impl AtomicWaker {
    pub const fn new() -> Self {
        Self {
            state: AtomicUsize::new(0),
            waker: UnsafeCell::new(None),
        }
    }

    pub fn poll(&self, waker_ref: &Waker, waiting: impl FnOnce()) -> Poll<usize> {
        
        
    }

    pub fn detach(&self) -> bool {
        const NOOP_VTABLE: RawWakerVtable = RawWakerVtable::new(

        );

        self.poll(&waker, || {}) != Poll::Pending
    }

    pub fn reset(&self, token: usize) -> bool {
        
    }

    pub fn wake(&self) -> Option<Waker> {
        
    }
}
