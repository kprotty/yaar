use super::scheduler::{
    executor::Executor,
    task::{Task, TaskJoinable},
};
use std::{future::Future, num::NonZeroUsize, sync::Arc};

#[derive(Default)]
pub struct Builder {
    worker_threads: Option<NonZeroUsize>,
}

impl Builder {
    pub const fn new() -> Self {
        Self {
            worker_threads: None,
        }
    }

    pub fn worker_threads(&mut self, worker_threads: usize) -> &mut Self {
        self.worker_threads = NonZeroUsize::new(worker_threads);
        self
    }

    pub fn block_on<F>(&self, future: F) -> F::Output
    where
        F: Future + Send + 'static,
        F::Output: Send + 'static,
    {
        let executor = Executor::new(self.worker_threads).unwrap();
        let executor = Arc::new(executor);

        let task = Task::spawn(future, &executor, None);
        task.join()
    }
}
