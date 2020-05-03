use super::{Platform, Worker};
use core::{
    pin::Pin,
    ptr::NonNull,
    marker::PhantomPinned,
    sync::atomic::AtomicPtr,
};

#[derive(Copy, Clone, Eq, PartialEq)]
pub enum TaskPriority {
    Low,
    Normal,
    High,
    Critical,
}

pub type TaskResumeFn<P> = unsafe fn(
    Pin<&mut Task<P>>,
    Pin<&Worker<P>>
) -> Option<Pin<&mut Task<P>>>;

#[repr(align(16))]
pub struct Task<P: Platform> {
    _pinned: PhantomPinned,
    state: AtomicPtr<Self>,
    resume: NonNull<TaskResumeFn<P>>,
}

impl<P: Platform> Task<P> {
    #[inline]
    pub fn new(resume: NonNull<TaskResumeFn<P>>) -> Self {
        Self {
            _pinned: PhantomPinned,
            state: AtomicPtr::default(),
            resume,
        }
    }

    #[inline]
    pub unsafe fn resume(self: Pin<&mut Self>, worker: Pin<&Worker<P>>) {
        (*self.resume.as_ref())(self, worker)
    }
}
