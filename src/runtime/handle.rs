use super::scheduler::{
    config::Config,
    context::{Context, ContextRef},
    executor::Executor,
    task::Task,
    worker::WorkerContext,
};
use crate::task::JoinHandle;
use std::{future::Future, io, sync::Arc};

pub struct Handle {
    pub(crate) executor: Arc<Executor>,
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
        Executor::from(config).map(|executor| Self {
            executor: Arc::new(executor),
        })
    }

    pub fn current() -> Self {
        Self {
            executor: Context::current().as_ref().executor.clone(),
        }
    }

    pub fn try_current() -> Result<Self, ()> {
        Context::try_current().ok_or(()).map(|context_ref| Self {
            executor: context_ref.as_ref().executor.clone(),
        })
    }

    pub fn spawn<F>(&self, future: F) -> JoinHandle<F::Output>
    where
        F: Future + Send + 'static,
        F::Output: Send + 'static,
    {
        let context_ref = Context::try_current();
        let context = context_ref.as_ref().map(|c| c.as_ref());

        let task = Task::spawn(future, &self.executor, context);
        JoinHandle {
            joinable: Some(task),
        }
    }

    pub fn block_on<F: Future>(&self, future: F) -> F::Output {
        WorkerContext::block_on(&self.executor, None, future)
    }

    pub fn enter(&self) -> EnterGuard<'_> {
        EnterGuard {
            _handle: self,
            _context_ref: Context::enter(&self.executor),
        }
    }
}

pub struct EnterGuard<'a> {
    _handle: &'a Handle,
    _context_ref: ContextRef,
}
