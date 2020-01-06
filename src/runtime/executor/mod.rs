
mod serial;
mod parallel;

use super::Config;

pub enum Error {}

pub enum Executor {
    Serial(SerialExecutor),
    Parallel(ParallelExecutor),
}

impl Executor {
    pub fn run<F>(, future: F) -> Result<F::Output, Error>
    where
        F: Future + Send + 'static,
        F::Output: Send + 'static,
    {
        Executor::run(self, future)
    }
}
