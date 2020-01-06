
use super::Config;
use core::{
    cell::Cell,
    ptr::NonNull,
    alloc::GlobalAlloc,
};

pub enum Error {
    TlsOutOfMemory,
}

pub enum Executor {
    Serial(SerialExecutor),
    Parallel(ParallelExecutor),
}

impl Executor {
    pub fn run<A, F>(config: &Config, allocator: &A, future: F) -> Result<F::Output, Error>
    where
        A: GlobalAlloc,
        F: Future + Send + 'static,
        F::Output: Send + 'static,
    {
        match config.max_threads {
            1 => SerialExecutor::run(config, allocator, future),
            _ => ParallelExecutor::run(config, allocator, future),
        }
    }
}
