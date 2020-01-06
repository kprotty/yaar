use super::executor::{Error, Executor};
use std::future::Future;

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
        self.max_threads = thread_count;
        self
    }

    pub fn max_async_threads(&mut self, thread_count: NonZeroUsize) -> &mut Self {
        self.max_async_threads = thread_count;
        self
    }

    pub fn thread_stack_size(&mut self, stack_size: Option<NonZeroUsize>) -> &mut Self {
        self.thread_stack_size = stack_size;
        self
    }

    pub fn run<F>(&self, future: F) -> Result<F::Output, Error>
    where
        F: Future + Send + 'static,
        F::Output: Send + 'static,
    {
        Executor::run(self, future)
    }
}