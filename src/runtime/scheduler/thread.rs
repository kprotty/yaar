use super::{config::Config, context::Context, executor::Executor, worker::WorkerContext};
use parking_lot::{Condvar, Mutex};
use std::{time::Duration, mem::{replace, drop}, future::Future, pin::Pin, task::{Wake, Poll, Waker, Context}, sync::Arc, thread};


#[derive(Copy, Clone, Eq, PartialEq, Debug)]
pub enum Notified {
    Shutdown,
    TimedOut,
    Signaled(usize),
}

#[derive(Copy, Clone, Eq, PartialEq, Debug)]
enum ParkState {
    Empty,
    Waiting((usize, bool))
    Notified(usize),
}

pub struct Parker {
    pub task_state: TaskState,
    executor: Arc<Executor>,
    state: AtomicUsize,
}

impl Parker {
    pub fn poll<F: Future>(self: &Arc<Self>, context: &Context, future: Pin<&mut F>) -> Poll<F::Output> {

    }

    fn park(&self, timeout: Option<Duration>) {

    }

    fn unpark(&self) {

    }
}

struct PoolState {
    spawned: usize,
    shutdown: bool,
    parkers: Vec<Arc<Parker>>,
}

struct ThreadPool {
    pub config: Config,
    max_threads: NonZeroUsize,
    state: Mutex<PoolState>,
}

impl ThreadPool {
    pub fn from(config: Config) -> Self {
        let max_threads = config.max_threads().unwrap();
        Self {
            config,
            max_threads,
            state: Mutex::new(PoolState {
                spawned: 0,
                shutdown: false,
                notified: Vec::with_capacity(max_threads.get()),
            }),
        }
    }

    pub fn spawn(&self, executor: &Arc<Executor>, worker_index: usize) -> Result<(), ()> {
        let mut state = self.state.lock();
        if state.shutdown {
            return Err(());
        }

        if let Some(top_index) = state.parkers.len().checked_sub(1) {
            let parker = state.parkers.swap_remove(top_index);
            assert!(Arc::ptr_eq(&parker.executor, executor));

            let park_state = parker.state.load(Ordering::Relaxed);
            let (index, polling) = match ParkState::from(park_state) {
                ParkState::Waiting(waiting) => waiting,
                ParkState::Empty => unreachable!("Parker waiting with invalid state"),
                ParkState::Notified(_) => unreachable!("Parker notified when still waiting"),
            };

            let park_state = ParkState::Notified(Some(worker_index));
            parker.state.store(park_state.into(), Ordering::Relaxed);
            drop(state);

            assert_eq!(index, top_index);
            if polling {
                parker.executor.io_driver.notify();
            } else {
                parker.unpark();
            }

            return Ok(());
        }

        if state.spawned == self.max_threads.get() {
            return Err(());
        }

        state.spawned += 1;
        drop(state);

        let executor = executor.clone();
        thread::Builder::new()
            .name(self.config.on_thread_name.as_ref().unwrap()())
            .stack_size(self.config.stack_size.unwrap().get())
            .spawn(move || Self::run(&executor, Some(worker_index)))
            .map(drop)
            .map_err(|_| self.finish())
    }

    pub fn run(executor: &Arc<Executor>, worker_index: Option<usize>) {
        if let Some(on_thread_start) = self.config.on_thread_start.as_ref() {
            (on_thread_start)();
        }

        

        if let Some(on_thread_stop) = self.config.on_thread_stop.as_ref() {
            (on_thread_stop)();
        }
    }

    pub fn wait(&self, parker: &Arc<Parker>, timeout: Option<Duration>) -> Notified {
        let mut state = self.state.lock();
        if state.shutdown {
            return Notified::Shutdown;
        }

        let park_state = parker.state.load(Ordering::Relaxed);
        match ParkState::from(park_state) {
            ParkState::Waiting(_waiting) => unreachable!("Parker waiting on multiple threads"),
            ParkState::Notified(Some(worker_index)) => Notified::Signaled(worker_index),
            ParkState::Notified(None) => return Notified::Unparked,
            ParkState::Empty => {},
        }

        let poll_guard = parker.executor.io_driver.try_poll();
        let polling = poll_guard.is_some();

        let index = state.parkers.len();
        let park_state = ParkState::Waiting((index, polling));
        parker.state.store(park_state.into(), Ordering::Relaxed);

        state.parkers.push(parker.clone());
        drop(state);

        if let Some(on_thread_park) = self.config.on_thread_park.as_ref() {
            (on_thread_park)();
        }

        match poll_guard {
            Some(guard) => guard.poll(timeout),
            None => parker.park(timeout)
        }

        if let Some(on_thread_unpark) = self.config.on_thread_unpark.as_ref() {
            (on_thread_unpark)();
        }

        state = self.state.lock();
        if state.shutdown {
            return Notified::Shutdown;
        }

        let park_state = parker.state.load(Ordering::Relaxed);
        let notified = match ParkState::from(park_state) {
            ParkState::Waiting((index, _polling)) => {
                let p = state.parkers.swap_remove(index);
                assert!(Arc::ptr_eq(&p, parker));
                Notified::TimedOut
            },
            ParkState::Notified(None) => Notified::Unparked,
            ParkState::Notified(Some(worker_index)) => Notified::Signaled(worker_index),
            ParkState::Empty => unreachable!("Parker woke up with invalid state"),
        };

        let park_state = ParkState::Empty;
        parker.state.store(park_state.into(), Ordering::Relaxed);
        notified
    }

    fn notify(&self, parker: &Arc<Parker>) {
        let mut state = self.state.lock();
        if state.shutdown {
            return;
        }

        let park_state = parker.state.load(Ordering::Relaxed);
        let old_park_state = ParkState::from(park_state);

        let new_park_state = ParkState::Notified(None);
        parker.state.store(new_park_state.into(), Ordering::Relaxed);

        let (index, polling) = match ParkState::from(old_park_state) {
            ParkState::Waiting(waiting) => waiting,
            _ => return,
        };

        let old_parker = state.parkers.swap_remove(index);
        assert!(Arc::ptr_eq(&old_parker, parker));
        drop(state);

        if polling {
            parker.executor.io_driver.notify();
        } else {
            parker.unpark();
        }
    }

    pub fn shutdown(&self) {
        let mut state = self.state.lock();
        if replace(&mut state.shutdown, true) {
            return;
        }

        let mut idle = replace(&mut state.idle, Vec::new());
        drop(state);

        for parker in idle.into_iter() {
            let park_state = parker.state.load(Ordering::Relaxed);
            let (_index, polling) = match ParkState::from(park_state) {
                ParkState::Waiting(waiting) => waiting,
                _ => unreachable!("Parker with invalid state when shutting down")
            };

            if polling {
                parker.executor.io_driver.notify();
            } else {
                parker.unpark();
            }
        }
    }
}
impl ThreadPool {
    

    pub fn spawn(&self, executor: &Arc<Executor>, worker_index: usize) -> Result<(), ()> {
        let mut state = self.state.lock();
        if state.shutdown {
            return Err(());
        }

        if state.notified.len() < state.idle {
            state.notified.push_back(worker_index);
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
