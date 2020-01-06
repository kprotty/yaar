
mod config;
mod executor;

pub use config::Config;

pub mod task {
    use core::future::Future;
    use super::executor::Executor;

    pub fn spawn<F>(future: F) -> JoinHandle<F::Output>
    where
        F: Future + Send + 'static,
        F::Output: Send + 'static,
    {
        Executor::get().unwrap(). 
    }
}
