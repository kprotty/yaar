use super::{config::Config, parker::Parker};
use std::{
    sync::atomic::{AtomicBool, Ordering},
    sync::{Arc, Mutex},
};

struct Inner {
    spawned: usize,
    idle: Vec<Arc<Parker>>,
}

pub struct ThreadPool {
    config: Config,
    running: AtomicBool,
    inner: Mutex<Inner>,
}

impl From<Config> for ThreadPool {
    fn from(config: Config) -> Self {
        Self {
            config,
            running: AtomicBool::new(true),
            inner: Mutex::new(Inner {
                spawned: 0,
                idle: Vec::new(),
            }),
        }
    }
}

impl ThreadPool {
    pub(super) fn spawn(&self, executor: &Arc<Executor>, worker_index: usize) -> Result<(), ()> {
        loop {
            let mut inner = self.inner.lock().unwrap();
            if !self.running.load(Ordering::Relaxed) {
                return Err(());
            }

            if let Some(parker_index) = inner.idle.len().checked_sub(1) {
                let parker = inner.idle.swap_remove(parker_index);
                drop(inner);

                if parker.unpark(Some(worker_index)) {
                    return Ok(());
                } else {
                    continue;
                }
            }

            if inner.spawned == self.max_threads() {
                return Err(());
            }

            inner.spawned += 1;
            drop(inner);

            let executor = executor.clone();
            return thread::Builder::new()
                .stack_size(self.config.stack_size.get())
                .name((self.config.thread_name)())
                .spawn(move || executor.thread_pool.run(&executor, worker_index))
                .map_err(|_| self.finish())
                .map(drop);
        }
    }

    fn run(&self, executor: &Arc<Executor>, worker_index: usize) {
        (self.config.on_thread_start)();
        
        (self.config.on_thread_stop)();
    }

    fn max_threads(&self) -> usize {
        let blocking_threads = self.config.blocking_threads.get();
        let worker_threads = self.config.worker_threads.get();
        worker_threads + blocking_threads
    }

    fn finish(&self) {
        let mut inner = self.inner.lock().unwrap();
        assert!(inner.spawned <= self.max_threads());
        assert_ne!(inner.spawned, 0);
        inner.spawned -= 1;
    }
}
