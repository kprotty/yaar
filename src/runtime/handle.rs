use super::scheduler::{
    config::Config,
    context::Context,
    executor::Executor,
    task::{JoinHandle, Task},
    worker::Worker,
};
use std::{future::Future, io, marker::PhantomData, sync::Arc};

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
        Executor::build(config).map(|executor| Self {
            executor: Arc::new(executor),
        })
    }

    pub fn current() -> Self {
        Self {
            executor: Context::current().executor.clone(),
        }
    }

    pub fn try_current() -> Result<Self, ()> {
        Context::try_current().ok_or(()).map(|context| Self {
            executor: context.executor.clone(),
        })
    }

    pub fn spawn<F>(&self, future: F) -> JoinHandle<F::Output>
    where
        F: Future + Send + 'static,
        F::Output: Send + 'static,
    {
        let context = Context::try_current();
        let task = Task::spawn(&self.executor, context, future);
        JoinHandle(Some(task))
    }

    pub fn spawn_blocking<F, R>(&self, func: F) -> JoinHandle<R>
    where
        F: FnOnce() -> R + Send + 'static,
        R: Send + 'static,
    {
        unimplemented!("todo")
    }

    pub fn block_on<F: Future>(&self, future: F) -> F::Output {
        pin_utils::pin_mut!(future);
        Worker::block_on(&self.executor, None, future)
    }

    pub fn enter<'a>(&'a self) -> EnterGuard<'a> {
        EnterGuard {
            _context: Context::enter(&self.executor),
            _handle: PhantomData,
        }
    }
}

pub struct EnterGuard<'a> {
    _context: Context,
    _handle: PhantomData<&'a Handle>,
}
