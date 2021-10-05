use super::{
    worker::{Worker, WorkerRef, WorkerTask},
    builder::Builder,
};
use std::{
    sync::Arc,
    sync::atomic::{AtomicUsize, Ordering},
};

pub(crate) struct Pool {
    sync: AtomicUsize,
    injecting: AtomicUsize,
    pub(super) workers: Pin<Box<[Worker]>>,
}

impl From<Builder> for Pool {
    fn from(builder: Builder) -> Self {

    }
}

impl Pool {
    pub(super) fn mark_task_begin(&self) {

    }

    pub(super) fn mark_task_complete(&self) {

    }

    pub(super) unsafe fn schedule<'a>(self: &Arc<Self>, worker_task: WorkerTask<'a>) {
        let worker_index = self.injecting.fetch_add(1, Ordering::Relaxed);
        let worker_index = worker_index % self.workers.len();
        WorkerRef::push(self, worker_index, worker_task)
    }
}