pub mod list;

pub mod future;

use super::Worker;
use core::{
    pin::Pin,
    ptr::NonNull,
    marker::PhantomPinned,
    sync::atomic::AtomicPtr,
};

#[derive(Copy, Clone, Eq, PartialEq)]
pub enum TaskLocality {
    Worker,
    Node,
    Scheduler,
}

#[derive(Copy, Clone, Eq, PartialEq)]
pub enum TaskPriority {
    Low,
    Normal,
    High,
    Critical,
}

pub type TaskResumeFn = unsafe fn(
    Pin<&mut Task>,
    Pin<&Worker>
) -> Option<Pin<&mut Task>>;

#[repr(align(4))]
pub struct Task {
    _pinned: PhantomPinned,
    state: AtomicPtr<Self>,
    pub(crate) resume: NonNull<TaskResumeFn>,
}

impl Task {
    #[inline]
    pub fn new(resume: NonNull<TaskResumeFn>) -> Self {
        Self {
            _pinned: PhantomPinned,
            state: AtomicPtr::default(),
            resume,
        }
    }

    #[inline]
    pub unsafe fn resume(self: Pin<&mut Self>, worker: Pin<&Worker>) {
        (*self.resume.as_ref())(self, worker)
    }

    #[inline]
    pub(crate) fn decode(task: NonNull<Self>) -> (NonNull<Self>, TaskPriority) {
        let value = task.as_ptr() as usize;
        let ptr = (value & !0b11) as *mut Self;
        let priority = match value & 0b11 {
            0b00 => TaskPriority::Normal,
            0b01 => TaskPriority::Low,
            0b10 => TaskPriority::High,
            0b11 => TaskPriority::Critical,
        };

        (NonNull::new_unchecked(ptr), priority)
    }

    #[inline]
    pub(crate) fn encode(task: NonNull<Self>, priority: TaskPriority) -> NonNull<Self> {
        let mut value = task.as_ptr() as usize;
        value |= match priority {
            TaskPriority::Normal => 0b00,
            TaskPriority::Low => 0b01,
            TaskPriority::High => 0b10,
            TaskPriority::Critical => 0b11, 
        };

        NonNull::new_unchecked(value as *mut Self)
    }
}
