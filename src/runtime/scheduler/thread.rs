use super::{config::Config, context::Context, executor::Executor, worker::WorkerContext};
use parking_lot::{Condvar, Mutex};
use std::{collections::VecDeque, mem::replace, sync::Arc, thread};

struct PoolState {
    idle: usize,
    spawned: usize,
    shutdown: bool,
    notified: VecDeque<usize>,
}

pub struct ThreadPool {
    pub config: Config,
    condvar: Condvar,
    state: Mutex<PoolState>,
}

impl ThreadPool {
    pub fn from(config: Config) -> Self {
        let max_threads = config.max_threads().unwrap().get();
        Self {
            config,
            condvar: Condvar::new(),
            state: Mutex::new(PoolState {
                idle: 0,
                spawned: 0,
                shutdown: false,
                notified: VecDeque::with_capacity(max_threads),
            }),
        }
    }

    pub fn spawn(&self, executor: &Arc<Executor>, worker_index: usize) -> Result<(), ()> {
        let mut state = self.state.lock();
        if state.shutdown {
            return Err(());
        }

        if state.notified.len() < state.idle {
            state.notified.push_back(worker_index);
            drop(state);
            self.condvar.notify_one();
            return Ok(());
        }

        let max_threads = self.config.max_threads().unwrap().get();
        assert!(state.spawned <= max_threads);
        if state.spawned == max_threads {
            return Err(());
        }

        let position = state.spawned;
        state.spawned += 1;
        drop(state);

        if position == 0 {
            Self::run(executor, worker_index);
            return Ok(());
        }

        let executor = executor.clone();
        thread::Builder::new()
            .name(self.config.on_thread_name.as_ref().unwrap()())
            .stack_size(self.config.stack_size.unwrap().get())
            .spawn(move || Self::run(&executor, worker_index))
            .map(drop)
            .map_err(|_| self.finish())
    }

    fn run(executor: &Arc<Executor>, worker_index: usize) {
        let context_ref = Context::enter(executor);
        WorkerContext::run(context_ref.as_ref(), worker_index);
        executor.thread_pool.finish();
    }

    fn finish(&self) {
        let mut state = self.state.lock();

        let max_threads = self.config.max_threads().unwrap().get();
        assert!(state.spawned <= max_threads);

        assert_ne!(state.spawned, 0);
        state.spawned -= 1;
    }

    pub fn wait(&self) -> Result<usize, ()> {
        let mut state = self.state.lock();
        loop {
            if state.shutdown {
                return Err(());
            }

            if let Some(worker_index) = state.notified.pop_back() {
                return Ok(worker_index);
            }

            let max_threads = self.config.max_threads().unwrap().get();
            state.idle += 1;
            assert!(state.idle <= max_threads);

            self.condvar.wait(&mut state);

            assert!(state.idle <= max_threads);
            assert_ne!(state.idle, 0);
            state.idle -= 1;
        }
    }

    pub fn shutdown(&self) {
        let mut state = self.state.lock();
        if replace(&mut state.shutdown, true) {
            return;
        }

        if state.idle > 0 && state.notified.len() < state.idle {
            self.condvar.notify_all();
        }
    }
}
