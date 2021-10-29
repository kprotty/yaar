use super::{executor::Executor, thread::Thread};
use parking_lot::{Condvar, Mutex};
use std::{
    collections::VecDeque,
    mem,
    num::NonZeroUsize,
    sync::atomic::{AtomicBool, Ordering},
    sync::Arc,
    thread,
    time::{Duration, Instant},
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
    pub keep_alive: Option<Duration>,
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
        if let Some(callback) = executor.thread_pool.config.on_thread_start.as_ref() {
            (callback)();
        }

        Thread::run(&executor, notified);
        executor.thread_pool.finish();

        if let Some(callback) = executor.thread_pool.config.on_thread_stop.as_ref() {
            (callback)();
        }
    }

    fn finish(&self) {
        let mut pool = self.pool.lock();
        assert_ne!(pool.spawned, 0);
        pool.spawned -= 1;
    }

    pub fn wait(&self, deadline: Option<Instant>) -> Result<Option<Notified>, ()> {
        if self.shutdown.load(Ordering::Acquire) {
            return Err(());
        }

        let keep_alive = self.config.keep_alive.unwrap_or(Duration::from_secs(10));
        let force_deadline = deadline.unwrap_or_else(|| Instant::now() + keep_alive);

        let mut pool = self.pool.lock();
        assert_ne!(pool.spawned, 0);

        let mut timed_out = false;
        loop {
            if self.shutdown.load(Ordering::Acquire) {
                return Err(());
            }

            if let Some(notified) = pool.notified.pop_back() {
                return Ok(Some(notified));
            }

            if timed_out {
                return match deadline {
                    Some(_) => Ok(None),
                    None => Err(()),
                };
            }

            pool.idle += 1;
            timed_out = self.cond.wait_until(&mut pool, force_deadline).timed_out();
            pool.idle -= 1;
        }
    }

    pub fn shutdown(&self) {
        self.shutdown.store(true, Ordering::Release);

        let _pool = self.pool.lock();
        let _ = self.cond.notify_all();
    }
}
