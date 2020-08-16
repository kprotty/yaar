use super::{Pool, Thread, ThreadHandle};
use core::{num::NonZeroUsize, ptr::NonNull, sync::atomic::AtomicUsize};

#[repr(align(8))]
pub struct Worker {
    ptr: AtomicUsize,
}

pub enum WorkerType {
    Worker(Option<NonZeroUsize>),
    Thread(NonNull<Thread>),
    Pool(NonNull<Pool>),
    Handle(ThreadHandle),
}

impl From<usize> for WorkerType {
    fn from(value: usize) -> Self {
        unsafe {
            let ptr = value & !0b11usize;
            match value & 0b11 {
                0 => Self::Thread(NonNull::new_unchecked(ptr as *mut Thread)),
                1 => Self::Worker(NonZeroUsize::new(value >> 2)),
                2 => Self::Pool(NonNull::new_unchecked(ptr as *mut Pool)),
                3 => Self::Handle(ThreadHandle::new_unchecked(ptr)),
                _ => unreachable!(),
            }
        }
    }
}

impl Into<usize> for WorkerType {
    fn into(self) -> usize {
        match self {
            Self::Thread(thread) => (thread.as_ptr() as usize) | 0,
            Self::Worker(index) => (index.map(|i| i.get()).unwrap_or(0) >> 2) | 1,
            Self::Pool(pool) => (pool.as_ptr() as usize) | 2,
            Self::Handle(handle) => usize::from(handle) | 3,
        }
    }
}
