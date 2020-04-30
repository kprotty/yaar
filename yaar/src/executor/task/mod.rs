mod waker;
pub use waker::*;

mod future;
pub use future::*;

mod join_handle;
pub use join_handle::*;

mod poll_context;
pub use poll_context::*;

use super::{Platform, Worker};
use core::{
    pin::Pin,
    sync::atomic::AtomicPtr,
};

pub(crate) type TaskResumeFn<P> = fn(&mut Task<P>, Pin<&Worker<P>>);

pub struct Task<P: Platform> {
    state: AtomicPtr<Self>,
    resume: TaskResumeFn<P>,
}

impl<P: Platform> From<TaskResumeFn<P>> for Task<P> {
    fn from(resume: TaskResumeFn<P>) -> Self {
        Self::new(resume)
    }
}

impl<P: Platform> Task<P> {
    #[inline]
    pub fn new(resume: TaskResumeFn<P>) -> Self {
        Self {
            state: AtomicPtr::default(),
            resume,
        }
    }

    #[inline]
    pub fn resume(&mut self, worker: Pin<&Worker<P>>) {
        (self.resume)(self, worker)
    }
}
