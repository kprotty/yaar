mod builder;
mod enter;
mod handle;

pub(crate) mod scheduler;

pub use builder::Builder;
pub use enter::EnterGuard;
pub use handle::Handle;

use scheduler::{task::JoinHandle, Config};
use std::{future::Future, io};

pub struct Runtime {
    handle: Handle,
}

impl Runtime {
    pub fn new() -> io::Result<Self> {
        Builder::new().build()
    }

    pub(crate) fn from_config(config: Config) -> io::Result<Self> {
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

    pub fn block_on<F: Future>(&self, future: F) -> F::Output {
        self.handle.block_on(future)
    }

    pub fn enter(&self) -> EnterGuard<'_> {
        self.handle.enter()
    }
}
