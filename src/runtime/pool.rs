use super::{
    builder::Builder,
    task::Task,
    worker::{Worker, WorkerRef, WorkerTask},
};
use std::{
    sync::atomic::{AtomicUsize, Ordering},
    sync::Arc,
};

pub(crate) struct Pool {
    sync: AtomicUsize,
    pending: AtomicUsize,
    injecting: AtomicUsize,
    pub(super) workers: Pin<Box<[Worker]>>,
}

impl From<Builder> for Pool {
    fn from(builder: Builder) -> Self {}
}

impl Pool {
    pub(super) fn mark_task_begin(&self) {}

    pub(super) fn mark_task_complete(&self) {}

    /// Schedules a WorkerTask for execution.
    ///
    /// # Safety
    ///
    /// The WorkerTask must live long enough to be scheduled.
    /// Otherwise, a use-after-free could occur inside the runtime.
    pub(super) unsafe fn schedule<'a>(self: &Arc<Self>, worker_task: WorkerTask<'a>) {
        let worker_index = self.injecting.fetch_add(1, Ordering::Relaxed);
        let worker_index = worker_index % self.workers.len();
        WorkerRef::push(self, worker_index, worker_task)
    }
}
