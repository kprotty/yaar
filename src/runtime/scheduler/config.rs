use std::{num::NonZeroUsize, time::Duration};

pub struct Config {
    pub keep_alive: Duration,
    pub stack_size: NonZeroUsize,
    pub worker_threads: NonZeroUsize,
    pub blocking_threads: NonZeroUsize,
    pub thread_name: Arc<dyn Fn() -> String + Send + Sync + 'static>,
    pub on_thread_start: Arc<dyn Fn() + Send + Sync + 'static>,
    pub on_thread_stop: Arc<dyn Fn() + Send + Sync + 'static>,
    pub on_thread_park: Arc<dyn Fn() + Send + Sync + 'static>,
    pub on_thread_unpark: Arc<dyn Fn() + Send + Sync + 'static>,
}

#[derive(Default)]
pub struct ConfigBuilder {
    pub keep_alive: Option<Duration>,
    pub stack_size: Option<NonZeroUsize>,
    pub worker_threads: Option<NonZeroUsize>,
    pub blocking_threads: Option<NonZeroUsize>,
    pub thread_name: Option<Arc<dyn Fn() -> String + Send + Sync + 'static>>,
    pub on_thread_start: Option<Arc<dyn Fn() + Send + Sync + 'static>>,
    pub on_thread_stop: Option<Arc<dyn Fn() + Send + Sync + 'static>>,
    pub on_thread_park: Option<Arc<dyn Fn() + Send + Sync + 'static>>,
    pub on_thread_unpark: Option<Arc<dyn Fn() + Send + Sync + 'static>>,
}

impl ConfigBuilder {
    pub fn build(&self) -> Config {
        Config {
            keep_alive: self.keep_alive.unwrap_or_else(|| Duration::from_secs(10)),
            stack_size: self
                .stack_size
                .or_else(|| NonZeroUsize::new(2 * 1024 * 1024))
                .unwrap(),
            worker_threads: self
                .worker_threads
                .or_else(|| NonZeroUsize::new(num_cpus::get()))
                .or_else(|| NonZeroUsize::new(1))
                .unwrap(),
            blocking_threads: self
                .blocking_threads
                .or_else(|| NonZeroUsize::new(512))
                .unwrap(),
            thread_name: self
                .thread_name
                .as_ref()
                .map(|callback| callback.clone())
                .unwrap_or_else(|| Arc::new(|| String::from("yaar-runtime_worker"))),
            on_thread_start: self
                .on_thread_start
                .as_ref()
                .map(|callback| callback.clone())
                .unwrap_or_else(|| Arc::new(|| {})),
            on_thread_stop: self
                .on_thread_stop
                .as_ref()
                .map(|callback| callback.clone())
                .unwrap_or_else(|| Arc::new(|| {})),
            on_thread_park: self
                .on_thread_park
                .as_ref()
                .map(|callback| callback.clone())
                .unwrap_or_else(|| Arc::new(|| {})),
            on_thread_unpark: self
                .on_thread_unpark
                .as_ref()
                .map(|callback| callback.clone())
                .unwrap_or_else(|| Arc::new(|| {})),
        }
    }
}
