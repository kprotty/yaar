use super::{pool::Pool, task::TaskFuture, worker::WorkerRef};
use std::{num::NonZeroUsize, sync::Arc};

#[derive(Default, Debug, Copy, Clone)]
pub struct Builder {
    pub(super) max_threads: Option<NonZeroUsize>,
    pub(super) stack_size: Option<NonZeroUsize>,
}

impl Builder {
    pub const fn new() -> Self {
        Self {
            max_threads: None,
            stack_size: None,
        }
    }

    pub fn max_threads(mut self, max_threads: usize) -> Self {
        self.max_threads = NonZeroUsize::new(max_threads);
        self
    }

    pub fn stack_size(mut self, stack_size: usize) -> Self {
        self.stack_size = NonZeroUsize::new(stack_size);
        self
    }

    pub fn block_on(&self, future: F) -> F::Output
    where
        F: Future + Send,
        F::Output: Send,
    {
        let pool = Arc::new(Pool::from(self));
        let worker_ref = WorkerRef { pool, index: 0 };
        let join_handle = TaskFuture::spawn(&worker_ref, future);
        join_handle.consume()
    }
}
