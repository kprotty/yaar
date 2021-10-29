use super::{executor::Executor, thread::Thread};
use parking_lot::{Condvar, Mutex};
use std::{
    collections::VecDeque,
    mem,
    num::NonZeroUsize,
    sync::atomic::{AtomicBool, Ordering},
    sync::Arc,
    thread,
    time::Instant,
};

#[derive(Copy, Clone)]
pub struct Notified {
    pub worker_index: usize,
    pub searching: bool,
}

#[derive(Default)]
struct Pool {
    idle: usize,
    spawned: usize,
    notified: VecDeque<Notified>,
}

pub struct ThreadPoolConfig {
    pub stack_size: NonZeroUsize,
    pub max_threads: NonZeroUsize,
    pub on_thread_start: Box<dyn Fn() + Send + Sync + 'static>,
    pub on_thread_stop: Box<dyn Fn() + Send + Sync + 'static>,
    pub on_thread_park: Box<dyn Fn() + Send + Sync + 'static>,
    pub on_thread_unpark: Box<dyn Fn() + Send + Sync + 'static>,
    pub on_thread_name: Box<dyn Fn() -> String + Send + Sync + 'static>,
}

pub struct ThreadPool {
    config: ThreadPoolConfig,
    shutdown: AtomicBool,
    pool: Mutex<Pool>,
    cond: Condvar,
}

impl From<ThreadPoolConfig> for ThreadPool {
    fn from(config: ThreadPoolConfig) -> Self {
        Self {
            config,
            shutdown: AtomicBool::new(false),
            pool: Mutex::new(Pool::default()),
            cond: Condvar::new(),
        }
    }
}

impl ThreadPool {
    pub fn notify(&self, executor: &Arc<Executor>, notified: Notified) -> Option<()> {
        let mut pool = self.pool.lock();

        if pool.idle > 0 {
            pool.notified.push_back(notified);
            return Some(());
        }

        if pool.spawned == self.config.max_threads.get() {
            return None;
        }

        let position = pool.spawned;
        pool.spawned += 1;
        mem::drop(pool);

        let executor = Arc::clone(executor);
        if position == 0 {
            Self::run(executor, notified);
            return Some(());
        }

        thread::Builder::new()
            .name((self.config.on_thread_name)())
            .stack_size(self.config.stack_size.get())
            .spawn(move || Self::run(executor, notified))
            .map_err(|_| self.finish())
            .map(mem::drop)
            .ok()
    }

    fn run(executor: Arc<Executor>, notified: Notified) {
        let thread_executor = executor.clone();
        let thread_pool = &executor.thread_pool;

        (thread_pool.config.on_thread_start)();
        Thread::run(thread_executor, notified);
        thread_pool.finish();
        (thread_pool.config.on_thread_stop)();
    }

    fn finish(&self) {
        let mut pool = self.pool.lock();
        assert_ne!(pool.spawned, 0);
        pool.spawned -= 1;
    }

    pub fn wait(&self, deadline: Option<Instant>) -> Option<Notified> {
        unimplemented!("TODO")
    }

    pub fn is_shutdown(&self) -> bool {
        self.shutdown.load(Ordering::Acquire)
    }

    pub fn shutdown(&self) {
        unimplemented!("TODO")
    }
}
