use super::{Platform, Worker};
use core::{cell::Cell, ptr::NonNull};

pub enum ThreadState {
    Idle,
    Polling,
    Searching,
    Running,
}

pub struct Thread<P: Platform> {
    pub data: P::ThreadLocalData,
    pub(crate) next: Cell<Option<NonNull<Self>>>,
    pub(crate) state: Cell<ThreadState>,
    pub(crate) worker: Cell<Option<NonNull<Worker<P>>>>,
}

impl<P: Platform> Thread<P> {
    pub fn worker(&self) -> Option<&Worker<P>> {
        self.worker.get().map(|ptr| unsafe { &*ptr.as_ptr() })
    }

    pub extern "C" fn run(_worker: &Worker<P>) {
        // TODO
    }
}
