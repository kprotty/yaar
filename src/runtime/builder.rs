use super::executor::{Executor, pool::ThreadPoolConfig, task};
use std::{future::Future, num::NonZeroUsize, time::Duration};

#[derive(Default)]
pub struct Builder {
    
    pub(crate) config: ThreadPoolConfig,
}

impl Builder {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn worker_threads(&mut self, threads: usize) -> &mut Self {
        self.worker_threads = NonZeroUsize::new(threads);
        self
    }

    pub fn max_blocking_threads(&mut self, threads: usize) -> &mut Self {
        self.config.max_threads = NonZeroUsize::new(threads);
        self
    }

    pub fn thread_stack_size(&mut self, stack_size: usize) -> &mut Self {
        self.config.stack_size = NonZeroUsize::new(stack_size);
        self
    }

    pub fn thread_keep_alive(&mut self, duration: Duration) -> &mut Self {
        self.config.keep_alive = Some(duration);
        self
    }

    pub fn on_thread_park(&mut self, callback: impl Fn() + Send + Sync + 'static) -> &mut Self {
        self.config.on_thread_park = Some(Box::new(callback));
        self
    }

    pub fn on_thread_unpark(&mut self, callback: impl Fn() + Send + Sync + 'static) -> &mut Self {
        self.config.on_thread_unpark = Some(Box::new(callback));
        self
    }

    pub fn on_thread_start(&mut self, callback: impl Fn() + Send + Sync + 'static) -> &mut Self {
        self.config.on_thread_start = Some(Box::new(callback));
        self
    }

    pub fn on_thread_stop(&mut self, callback: impl Fn() + Send + Sync + 'static) -> &mut Self {
        self.config.on_thread_stop = Some(Box::new(callback));
        self
    }

    pub fn thread_name_fn(
        &mut self,
        callback: impl Fn() -> String + Send + Sync + 'static,
    ) -> &mut Self {
        self.config.on_thread_name = Some(Box::new(callback));
        self
    }

    pub fn thread_name(&mut self, name: impl Into<String>) -> &mut Self {
        let name: String = name.into();
        self.thread_name_fn(move || name.clone())
    }

    pub fn block_on<F: Future>(self, future: F) -> F::Output {
        let Self {
            worker_threads,
            config,
        } = self;

        let worker_threads = worker_threads
            .or_else(|| NonZeroUsize::new(num_cpus::get()))
            .or(NonZeroUsize::new(1))
            .unwrap();

        let executor = Executor::new(worker_threads, config);
        task::block_on(future, &executor)
    }
}
