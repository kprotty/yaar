mod builder;
mod context;
mod coop;
mod executor;
mod handle;
mod parker;
mod pool;
mod queue;
mod random;
mod task;
mod worker;

use std::{future::Future, io, time::Duration};

pub struct Runtime {
    pub(super) handle: Handle,
}

impl Drop for Runtime {
    fn drop(&mut self) {
        self.handle.join(None)
    }
}

impl Runtime {
    pub fn new() -> io::Result<Self> {
        Builder::new().build()
    }

    pub fn handle(&self) -> &Handle {
        &self.handle
    }

    pub fn spawn<F>(&self, future: F) -> JoinHandle<F::Output>
    where
        F: Future + Send + 'static,
        F::Output: Send + 'static,
    {
        self.handle.spawn(future)
    }

    pub fn block_on<F: Future>(&self, future: F) -> F::Output {
        self.handle.block_on(future)
    }

    pub fn enter(&self) -> EnterGuard<'_> {
        self.handle.enter()
    }

    pub fn shutdown_background(self) {
        self.shutdown_timeout(Duration::ZERO)
    }

    pub fn shutdown_timeout(self, duration: Duration) {
        let Self { handle } = self;
        handle.join(Some(duration))
    }
}
