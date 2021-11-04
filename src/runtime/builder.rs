use super::{scheduler::config::Config, Runtime};
use std::{io, num::NonZeroUsize, sync::Arc, time::Duration};

#[derive(Default)]
pub struct Builder {
    config: Config,
}

impl Builder {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn worker_threads(&mut self, threads: usize) -> &mut Self {
        self.config.worker_threads = NonZeroUsize::new(threads);
        self
    }

    pub fn max_blocking_threads(&mut self, threads: usize) -> &mut Self {
        self.config.blocking_threads = NonZeroUsize::new(threads);
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
        self.config.on_thread_park = Some(Arc::new(callback));
        self
    }

    pub fn on_thread_unpark(&mut self, callback: impl Fn() + Send + Sync + 'static) -> &mut Self {
        self.config.on_thread_unpark = Some(Arc::new(callback));
        self
    }

    pub fn on_thread_start(&mut self, callback: impl Fn() + Send + Sync + 'static) -> &mut Self {
        self.config.on_thread_start = Some(Arc::new(callback));
        self
    }

    pub fn on_thread_stop(&mut self, callback: impl Fn() + Send + Sync + 'static) -> &mut Self {
        self.config.on_thread_stop = Some(Arc::new(callback));
        self
    }

    pub fn thread_name_fn(
        &mut self,
        callback: impl Fn() -> String + Send + Sync + 'static,
    ) -> &mut Self {
        self.config.on_thread_name = Some(Arc::new(callback));
        self
    }

    pub fn thread_name(&mut self, name: impl Into<String>) -> &mut Self {
        let name: String = name.into();
        self.thread_name_fn(move || name.clone())
    }

    pub fn build(&mut self) -> io::Result<Runtime> {
        Runtime::from(Config {
            worker_threads: self
                .config
                .worker_threads
                .or_else(|| NonZeroUsize::new(num_cpus::get())),
            blocking_threads: self
                .config
                .blocking_threads
                .or_else(|| NonZeroUsize::new(512)),
            stack_size: self
                .config
                .stack_size
                .or_else(|| NonZeroUsize::new(2 * 1024 * 1024)),
            keep_alive: self
                .config
                .keep_alive
                .or_else(|| Some(Duration::from_secs(10))),
            on_thread_park: self
                .config
                .on_thread_park
                .as_ref()
                .map(|callback| callback.clone()),
            on_thread_unpark: self
                .config
                .on_thread_unpark
                .as_ref()
                .map(|callback| callback.clone()),
            on_thread_start: self
                .config
                .on_thread_start
                .as_ref()
                .map(|callback| callback.clone()),
            on_thread_stop: self
                .config
                .on_thread_stop
                .as_ref()
                .map(|callback| callback.clone()),
            on_thread_name: self
                .config
                .on_thread_name
                .as_ref()
                .map(|callback| callback.clone())
                .or_else(|| Some(Arc::new(|| String::from("yaar-runtime-worker")))),
        })
    }
}
