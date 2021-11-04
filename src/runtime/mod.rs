mod builder;
mod handle;
pub(crate) mod scheduler;

pub use builder::Builder;
pub use handle::{EnterGuard, Handle};

use crate::task::JoinHandle;
use scheduler::config::Config;
use std::{future::Future, io, time::Duration};

pub struct Runtime {
    handle: Handle,
}

impl Drop for Runtime {
    fn drop(&mut self) {
        self.shutdown_with(None)
    }
}

impl Runtime {
    pub fn new() -> io::Result<Self> {
        Builder::new().build()
    }

    pub(crate) fn from(config: Config) -> io::Result<Self> {
        let handle = Handle::new(config)?;
        Ok(Self { handle })
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

    pub fn block_on<F>(&self, future: F) -> F::Output
    where
        F: Future + Send + 'static,
        F::Output: Send + 'static,
    {
        self.handle.block_on(future)
    }

    pub fn enter(&self) -> EnterGuard<'_> {
        self.handle.enter()
    }

    pub fn shutdown_background(self) {
        self.shutdown_timeout(Duration::ZERO)
    }

    pub fn shutdown_timeout(mut self, timeout: Duration) {
        self.shutdown_with(Some(timeout))
    }

    fn shutdown_with(&mut self, timeout: Option<Duration>) {
        let executor = &self.handle.executor;
        executor.shutdown();
        executor.join(timeout)
    }
}
