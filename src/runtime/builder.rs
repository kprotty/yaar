use super::{scheduler::config::ConfigBuilder, Runtime};
use std::{io, num::NonZeroUsize, sync::Arc, time::Duration};

#[derive(Default)]
pub struct Builder {
    config_builder: ConfigBuilder,
}

impl Builder {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn worker_threads(&mut self, threads: usize) -> &mut Self {
        self.config_builder.worker_threads = NonZeroUsize::new(threads);
        self
    }

    pub fn max_blocking_threads(&mut self, threads: usize) -> &mut Self {
        self.config_builder.blocking_threads = NonZeroUsize::new(threads);
        self
    }

    pub fn thread_stack_size(&mut self, stack_size: usize) -> &mut Self {
        self.config_builder.stack_size = NonZeroUsize::new(stack_size);
        self
    }

    pub fn thread_keep_alive(&mut self, duration: Duration) -> &mut Self {
        self.config_builder.keep_alive = Some(duration);
        self
    }

    pub fn on_thread_park(&mut self, callback: impl Fn() + Send + Sync + 'static) -> &mut Self {
        self.config_builder.on_thread_park = Some(Arc::new(callback));
        self
    }

    pub fn on_thread_unpark(&mut self, callback: impl Fn() + Send + Sync + 'static) -> &mut Self {
        self.config_builder.on_thread_unpark = Some(Arc::new(callback));
        self
    }

    pub fn on_thread_start(&mut self, callback: impl Fn() + Send + Sync + 'static) -> &mut Self {
        self.config_builder.on_thread_start = Some(Arc::new(callback));
        self
    }

    pub fn on_thread_stop(&mut self, callback: impl Fn() + Send + Sync + 'static) -> &mut Self {
        self.config_builder.on_thread_stop = Some(Arc::new(callback));
        self
    }

    pub fn thread_name_fn(
        &mut self,
        callback: impl Fn() -> String + Send + Sync + 'static,
    ) -> &mut Self {
        self.config_builder.on_thread_name = Some(Arc::new(callback));
        self
    }

    pub fn thread_name(&mut self, name: impl Into<String>) -> &mut Self {
        let name: String = name.into();
        self.thread_name_fn(move || name.clone())
    }

    pub fn build(&mut self) -> io::Result<Runtime> {
        Runtime::build(self.config_builder.build())
    }
}
