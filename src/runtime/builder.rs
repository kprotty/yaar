use std::{future::Future, num::NonZeroUsize};

#[derive(Default)]
pub struct Builder {
    pub(super) workers: Option<NonZeroUsize>,
    pub(super) blocking: Option<NonZeroUsize>,
    pub(super) stack_size: Option<NonZeroUsize>,
    pub(super) name_gen: Option<Box<dyn Fn() -> String + Send + Sync + 'static>>,
}

impl Builder {
    pub fn new() -> Self {
        Self {
            workers: None,
            blocking: None,
            stack_size: None,
            name_gen: None,
        }
    }

    pub fn worker_threads(&mut self, thread_count: usize) -> &mut Self {
        self.workers = NonZeroUsize::new(thread_count);
        self
    }

    pub fn max_blocking_threads(&mut self, thread_count: usize) -> &mut Self {
        self.blocking = NonZeroUsize::new(thread_count);
        self
    }

    pub fn thread_stack_size(&mut self, bytes: usize) -> &mut Self {
        self.stack_size = NonZeroUsize::new(bytes);
        self
    }

    pub fn thread_name_gen(
        &mut self,
        name_gen: impl Fn() -> String + Send + Sync + 'static,
    ) -> &mut Self {
        self.name_gen = Some(Box::new(name_gen));
        self
    }

    pub fn block_on<F: Future>(&self, future: F) -> F::Output
    where
        F: Send,
        F::Output: Send,
    {
        unimplemented!("TODO")
    }
}
