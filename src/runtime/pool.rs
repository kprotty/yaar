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

#[derive(Default)]
pub struct ThreadPoolConfig {
    pub stack_size: Option<NonZeroUsize>,
    pub max_threads: Option<NonZeroUsize>,
    pub on_thread_start: Option<Box<dyn Fn() + Send + Sync + 'static>>,
    pub on_thread_stop: Option<Box<dyn Fn() + Send + Sync + 'static>>,
    pub on_thread_park: Option<Box<dyn Fn() + Send + Sync + 'static>>,
    pub on_thread_unpark: Option<Box<dyn Fn() + Send + Sync + 'static>>,
    pub on_thread_name: Option<Box<dyn Fn() -> String + Send + Sync + 'static>>,
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

        let max_spawn = self.config.max_threads.unwrap().get();
        if pool.spawned == max_spawn {
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

        let thread_stack_size = self
            .config
            .stack_size
            .or(NonZeroUsize::new(2 * 1024 * 1024))
            .unwrap()
            .get();

        let thread_name = self
            .config
            .on_thread_name
            .as_ref()
            .map(|f| f())
            .unwrap_or(String::from("yaar-runtime-worker"));

        thread::Builder::new()
            .name(thread_name)
            .stack_size(thread_stack_size)
            .spawn(move || Self::run(executor, notified))
            .map_err(|_| self.finish())
            .map(mem::drop)
            .ok()
    }

    fn run(executor: Arc<Executor>, notified: Notified) {
        let thread_executor = executor.clone();
        let thread_pool = &executor.thread_pool;

        if let Some(callback) = thread_pool.config.on_thread_start.as_ref() {
            (callback)();
        }

        Thread::run(thread_executor, notified);
        thread_pool.finish();

        if let Some(callback) = thread_pool.config.on_thread_stop.as_ref() {
            (callback)();
        }
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
