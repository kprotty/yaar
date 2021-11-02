use super::{
    scheduler::{
        task::{self, JoinHandle},
        Config, Executor, Thread,
    },
    EnterGuard,
};
use std::{future::Future, io, sync::Arc};

pub struct Handle {
    executor: Arc<Executor>,
}

impl Clone for Handle {
    fn clone(&self) -> Self {
        Self {
            executor: self.executor.clone(),
        }
    }
}

impl Handle {
    pub(crate) fn new(config: Config) -> io::Result<Self> {
        Executor::new(config).map(|executor| Self {
            executor: Arc::new(executor),
        })
    }

    pub fn current() -> Self {
        Self::try_current().expect("Caller not running inside a runtime Context")
    }

    pub fn try_current() -> Result<Self, ()> {
        Thread::try_with(|thread| Self {
            executor: thread.executor.clone(),
        })
        .ok_or(())
    }

    pub fn spawn<F>(&self, future: F) -> JoinHandle<F::Output>
    where
        F: Future + Send + 'static,
        F::Output: Send + 'static,
    {
        let mut future = Some(future);
        Thread::try_with(|thread| {
            let future = future.take().unwrap();
            task::spawn(future, self.executor.clone(), Some(thread))
        })
        .unwrap_or_else(|| {
            let future = future.take().unwrap();
            task::spawn(future, self.executor.clone(), None)
        })
    }

    pub fn block_on<F: Future>(&self, future: F) -> F::Output {
        task::block_on(future, self.executor.clone())
    }

    pub fn enter(&self) -> EnterGuard<'_> {
        EnterGuard::new(&self.executor)
    }
}
