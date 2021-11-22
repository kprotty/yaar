use super::{
    builder::Builder,
    handle::{EnterGuard, Handle},
    scheduler::{config::Config, task::JoinHandle},
};
use std::{future::Future, io, mem::forget, time::Duration};

pub struct Runtime {
    handle: Handle,
}

impl Drop for Runtime {
    fn drop(&mut self) {
        self.handle.executor.thread_pool.join(None);
    }
}

impl Runtime {
    pub fn new() -> io::Result<Self> {
        Builder::new().build()
    }

    pub(super) fn build(config: Config) -> io::Result<Self> {
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

    pub fn spawn_blocking<F, R>(&self, func: F) -> JoinHandle<R>
    where
        F: FnOnce() -> R + Send + 'static,
        R: Send + 'static,
    {
        self.handle.spawn_blocking(func)
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
        handle.executor.thread_pool.join(Some(duration));
    }
}
