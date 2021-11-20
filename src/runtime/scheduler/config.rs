use std::{
    num::NonZeroUsize,
    time::Duration,
};

pub struct Config {
    pub keep_alive: Duration,
    pub stack_size: NonZeroUsize,
    pub worker_threads: NonZeroUsize,
    pub blocking_threads: NonZeroUsize,
    pub thread_name: Box<dyn Fn() -> String + Send + Sync + 'static>,
    pub on_thread_start: Box<dyn Fn() + Send + Sync + 'static>>,
    pub on_thread_stop: Box<dyn Fn() + Send + Sync + 'static>>,
    pub on_thread_park: Box<dyn Fn() + Send + Sync + 'static>>,
    pub on_thread_unpark: Box<dyn Fn() + Send + Sync + 'static>>,
}

#[derive(Default)]
pub struct ConfigBuilder {
    pub keep_alive: Option<Duration>,
    pub stack_size: Option<NonZeroUsize>,
    pub worker_threads: Option<NonZeroUsize>,
    pub blocking_threads: Option<NonZeroUsize>,
    pub thread_name: Option<Box<dyn Fn() -> String + Send + Sync + 'static>>,
    pub on_thread_start: Option<Box<dyn Fn() + Send + Sync + 'static>>,
    pub on_thread_stop: Option<Box<dyn Fn() + Send + Sync + 'static>>,
    pub on_thread_park: Option<Box<dyn Fn() + Send + Sync + 'static>>,
    pub on_thread_unpark: Option<Box<dyn Fn() + Send + Sync + 'static>>,
}

impl ConfigBuilder {
    pub fn build(self) -> Config {
        let Self {
            keep_alive,
            stack_size,
            worker_threads,
            blocking_threads,
            thread_name,
            on_thread_start,
            on_thread_stop,
            on_thread_park,
            on_thread_unpark,
        } = self;

        Config {
            keep_alive: keep_alive
                .unwrap_or_else(|| Duration::from_secs(10)),
            stack_size: stack_size
                .or_else(|| NonZeroUsize::new(2 * 1024 * 1024))
                .unwrap(),
            worker_threads: worker_threads
                .or_else(|| NonZeroUsize::new(num_cpus::get()))
                .or_else(|| NonZeroUsize::new(1))
                .unwrap(),
            blocking_threads: blocking_threads
                .or_else(|| NonZeroUsize::new(512))
                .unwrap(),
            thread_name: thread_name
                .unwrap_or_else(|| Box::new(|| String::from("yaar-runtime_worker"))),
            on_thread_start: on_thread_start
                .unwrap_or_else(|| Box::new(|| {})),
            on_thread_stop: on_thread_stop
                .unwrap_or_else(|| Box::new(|| {})),
            on_thread_park: on_thread_park
                .unwrap_or_else(|| Box::new(|| {})),
            on_thread_unpark: on_thread_unpark
                .unwrap_or_else(|| Box::new(|| {})),
        }
    }
}