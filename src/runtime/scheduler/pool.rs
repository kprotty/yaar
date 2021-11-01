use super::executor::Executor;
use parking_lot::{Condvar, Mutex};
use std::{collections::VecDeque, mem, num::NonZeroUsize, time::Instant};

#[derive(Copy, Clone, Eq, PartialEq, Debug)]
pub enum NotifyError {
    Shutdown,
    OutOfResources,
}

#[derive(Copy, Clone, Eq, PartialEq, Debug)]
pub enum WaitError {
    Shutdown,
    TimedOut,
}

#[derive(Copy, Clone)]
pub struct Notified {
    pub worker_index: usize,
    pub searching: bool,
}

pub struct Config {
    pub keep_alive: Option<Duration>,
    pub stack_size: Option<NonZeroUsize>,
    pub worker_threads: Option<NonZeroUsize>,
    pub blocking_threads: Option<NonZeroUsize>,
    pub on_thread_start: Option<Box<dyn Fn() + Send + Sync + 'static>>,
    pub on_thread_stop: Option<Box<dyn Fn() + Send + Sync + 'static>>,
    pub on_thread_park: Option<Box<dyn Fn() + Send + Sync + 'static>>,
    pub on_thread_unpark: Option<Box<dyn Fn() + Send + Sync + 'static>>,
    pub on_thread_name: Option<Box<dyn Fn() -> String + Send + Sync + 'static>>,
}

#[derive(Default)]
struct State {
    idle: usize,
    spawned: usize,
    shutdown: bool,
    notified: VecDeque<Notified>,
}

pub struct ThreadPool {
    config: Config,
    cond: Condvar,
    state: Mutex<State>,
}

impl From<Config> for ThreadPool {
    fn from(config: Config) {
        Self {
            config,
            cond: Condvar::new(),
            state: Mutex::new(State::default()),
        }
    }
}

impl ThreadPool {
    pub fn notify(&self, executor: &Arc<Executor>, notified: Notified) -> Result<(), NotifyError> {
        let mut state = self.state.lock();
        if state.shutdown {
            return Err(NotifyError::Shutdown);
        }

        if state.idle > state.notified.len() {
            state.notified.push_back(notified);
            self.cond.notify_one();
            return Ok(());
        }

        let blocking_threads = self.config.blocking_threads.unwrap().get();
        let worker_threads = self.config.worker_threads.unwrap().get();
        let max_threads = blocking_threads + worker_threads;

        if state.spawned == max_threads {
            return Err(NotifyError::OutOfResources);
        }

        state.spawned += 1;
        mem::drop(state);

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
            .map(|callback| (callback)())
            .unwrap_or_else(|| String::from("yaar-runtime-worker"));

        let executor = executor.clone();
        thread::Builder::new()
            .name(thread_name)
            .stack_size(thread_stack_size)
            .spawn(move || Self::run(executor, notified))
            .map_err(|_| self.finish())
            .map(mem::drop)
            .map_err(|_| NotifyError::OutOfResources)
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
        let mut state = self.state.lock();
        assert_ne!(state.spawned, 0);
        state.spawned -= 1;
    }

    pub fn wait(&self, deadline: Option<Instant>) -> Result<Notified, WaitError> {
        if let Some(callback) = executor.thread_pool.config.on_thread_park.as_ref() {
            (callback)();
        }

        let wait_result = self.wait_until(deadline.unwrap_or_else(|| {
            let default_delay = Duration::from_secs(10);
            Instant::now() + default_delay
        }));

        if let Some(callback) = executor.thread_pool.config.on_thread_unpark.as_ref() {
            (callback)();
        }

        wait_result
    }

    fn wait_until(&self, deadline: Instant) -> Result<Notified, WaitError> {
        let mut state = self.state.lock();
        assert_ne!(state.spawned, 0);

        let mut timed_out = false;
        loop {
            if state.shutdown {
                return Err(WaitError::Shutdown);
            }

            if let Some(notified) = state.notified.pop_back() {
                return Ok(notified);
            }

            if timed_out {
                return Err(WaitError::TimedOut);
            }

            state.idle += 1;
            timed_out = self.cond.wait_until(&mut state, deadline).timed_out();
            state.idle -= 1;
        }
    }

    pub fn shutdown(&self) {
        let mut state = self.state.lock();
        assert_eq!(state.shutdown, false);
        state.shutdown = true;

        if state.idle > 0 && state.notified.len() < state.idle {
            self.cond.notify_all();
        }
    }
}
