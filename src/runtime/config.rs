use super::executor::{Error, Executor};
use core::{
    future::Future,
    alloc::GlobalAlloc,
};

pub struct Config {
    max_threads: usize,
    max_async_threads: usize,
    thread_stack_size: Option<usize>,
}

impl Default for Config {
    fn default() -> Self {
        Self::new()
    }
}

impl Config {
    pub fn new() -> Self {
        Self {
            max_threads: 1024,
            max_async_threads: num_cpus::get(),
            thread_stack_size: None,
        }
    }

    pub fn max_threads(&mut self, thread_count: NonZeroUsize) -> &mut Self {
        self.max_threads = thread_count.get();
        self
    }

    pub fn max_async_threads(&mut self, thread_count: NonZeroUsize) -> &mut Self {
        self.max_async_threads = thread_count.get().min(self.max_threads);
        self
    }

    pub fn thread_stack_size(&mut self, stack_size: Option<NonZeroUsize>) -> &mut Self {
        self.thread_stack_size = stack_size.map(|s| s.get());
        self
    }

    pub fn run<A, F>(&self, allocator: &A, future: F) -> Result<F::Output, Error>
    where
        A: GlobalAlloc,
        F: Future + Send + 'static,
        F::Output: Send + 'static,
    {
        Executor::run(self, future)
    }
}