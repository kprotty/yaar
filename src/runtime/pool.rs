use std::{
    sync::atomic::{AtomicBool, AtomicUsize, Ordering},
    sync::{Arc, Condvar, Mutex},
    time::Duration,
};

struct PoolState {
    spawned: usize,
    parked: Vec<Arc<Parker>>,
}

pub struct ThreadPool {
    tasks: AtomicUsize,
    running: AtomicBool,
    state: Mutex<PoolState>,
    joiner: Condvar,
    config: Config,
}

impl From<Config> for ThreadPool {
    fn from(config: Config) -> Self {
        Self {
            tasks: AtomicUsize::new(0),
            running: AtomicBool::new(true),
            state: Mutex::default(),
            joiner: Condvar::new(),
            config,
        }
    }
}

impl ThreadPool {
    pub fn active(&self) -> bool {
        self.running.load(Ordering::Acquire)
    }

    pub fn task_start(&self) {}

    pub fn task_complete(&self) {}

    pub fn park(&self, parker: &Arc<Parker>, timeout: Option<Duration>) -> Option<usize> {
        if let Some(worker_index) = parker.poll() {
            return worker_index;
        }

        let mut state = self.state.lock().unwrap();
        if let Some(worker_index) = parker.poll() {
            return worker_index;
        }

        if !self.active() {
            return None;
        }

        state.parked.push(parker.clone());
        drop(state);

        if let Some(on_thread_park) = self.config.on_thread_park.as_ref() {
            (on_thread_park)();
        }

        let timeout = timeout.or(self.config.keep_alive).unwrap();
        if let Some(worker_index) = parker.park(timeout) {
            return worker_index;
        }

        if let Some(on_thread_unpark) = self.config.on_thread_unpark.as_ref() {
            (on_thread_unpark)();
        }

        let mut state = self.state.lock().unwrap();
        for index in 0..state.parked.len() {
            if Arc::ptr_eq(&state.parked[index], parker) {
                drop(state.parked.swap_remove(index));
                break;
            }
        }

        drop(state);
        parker.poll().unwrap()
    }

    pub fn unpark(&self, worker_index: Option<usize>) -> Result<(), ()> {
        loop {
            let mut state = self.state.lock().unwrap();
            if !self.active() {
                return Err(());
            }

            if let Some(index) = state.parked.len().checked_sub(1) {
                let parker = state.parked.swap_remove(index);
                drop(state);

                if parker.unpark(worker_index) {
                    return Ok(());
                } else {
                    continue;
                }
            }

            if state.spawned == self.config.max_threads() {
                return Err(());
            }

            state.spawned += 1;
            break;
        }

        let executor = parker.clone_executor();
        thread::Builder::new()
            .stack_size(self.config.stack_size.unwrap())
            .name((self.config.on_thread_name.as_ref().unwrap())())
            .spawn(move || executor.thread_pool.run(&executor, worker_index))
            .map(drop)
            .map_err(|_| self.complete())
    }

    fn run(&self, executor: &Arc<Executor>, worker_index: Option<usize>) {}

    pub fn shutdown(&self) {
        let mut state = self.state.lock().unwrap();
        if !self.active() {
            return;
        }

        self.running.store(false, Ordering::Release);
        let parked = replace(&mut state.parked, Vec::new());
        self.joiner.notify_all();
        drop(state);

        for parker in parked.into_iter() {
            parker.unpark(None);
        }
    }

    pub fn join(&self, timeout: Duration) {
        let mut state = self.state.lock().unwrap();
        let _ = self
            .joiner
            .wait_timeout_while(state, timeout, |_| self.active());
    }
}
