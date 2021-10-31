mod builder;
mod scheduler;
mod enter;
mod handle;

pub use self::{
    builder::Builder,
    enter::EnterGuard,
    handle::Handle,
};

pub struct Runtime {
    handle: Handle,
}

impl Runtime {
    pub fn new() -> io::Result<Runtime> {

    }

    pub fn block_on<F: Future>(&self, future: F) -> F::Output {
        self.handle.block_on(future)
    }

    pub fn spawn<F>(&self, future: F) -> JoinHandle<F::Output>
    where
        F: Future + Send + 'static,
        F::Output: Send + 'static
    {
        self.handle.spawn(future)
    }

    pub fn enter(&self) -> EnterGuard<'_> {
        self.handle.enter()
    }

    pub fn shutdown_background(self) {

    }

    pub fn shutdown_timeout(self, timeout: Duration) {

    }
}