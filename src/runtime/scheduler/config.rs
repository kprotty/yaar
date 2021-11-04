use std::{num::NonZeroUsize, sync::Arc, time::Duration};

pub type Callback = Arc<dyn Fn() + Send + Sync>;

pub type ThreadNameFn = Arc<dyn Fn() -> String + Send + Sync + 'static>;

#[derive(Default)]
pub struct Config {
    pub keep_alive: Option<Duration>,
    pub stack_size: Option<NonZeroUsize>,
    pub worker_threads: Option<NonZeroUsize>,
    pub blocking_threads: Option<NonZeroUsize>,
    pub on_thread_start: Option<Callback>,
    pub on_thread_stop: Option<Callback>,
    pub on_thread_park: Option<Callback>,
    pub on_thread_unpark: Option<Callback>,
    pub on_thread_name: Option<ThreadNameFn>,
}

impl Config {
    pub fn max_threads(&self) -> Option<NonZeroUsize> {
        let mut num_threads = self.worker_threads?.get();

        if let Some(blocking_threads) = self.blocking_threads {
            num_threads += blocking_threads.get();
        }

        NonZeroUsize::new(num_threads)
    }
}
