mod event;
pub use event::*;

mod header;
use header::*;

mod future;
pub use future::*;

use super::{Platform, Worker};
use core::{
    pin::Pin,
    sync::atomic::{Ordering, AtomicUsize},
};

#[derive(Copy, Clone, Eq, PartialEq, Debug)]
pub enum Locality {
    Worker,
    Node = 0,
    Scheduler,
}

impl Default for Locality {
    fn default() -> Self {
        Self::Node
    }
}

#[derive(Copy, Clone, Eq, PartialEq, Debug)]
pub enum Priority {
    Low,
    Normal = 0,
    High,
}

impl Default for Priority {
    fn default() -> Self {
        Self::Normal
    }
}

#[derive(Default)]
#[repr(align(16))]
pub struct Task<P: Platform> {
    _pinned: PhantomPinned,
    state: AtomicUsize,
    resume: unsafe fn(&Self, Pin<&Worker<P>>),
}

impl<P: Platform> Task<P> {
    pub fn new(resume: unsafe fn(&Self, Pin<&Worker<P>>)) -> Self {
        Self {
            _pinned: PhantomPinned,
            resume,
            state: AtomicUsize::new()
        }
    }

    #[inline]
    pub unsafe fn resume(&self, worker: Pin<&Worker<P>>) {
        (self.resume)(self, worker)
    }