use super::{config::Config, executor::Executor, parker::Parker, worker::WorkerContext};
use parking_lot::Mutex;
use std::{
    future::Future,
    mem::replace,
    pin::Pin,
    sync::atomic::{AtomicUsize, Ordering},
    sync::Arc,
    task::{Context as PollContext, Poll},
    thread,
    time::Duration,
};

struct PoolState {
    spawned: usize,
    idle: Vec<Arc<Parker>>,
}

pub struct ThreadPool {
    pub config: Config,
    pending: AtomicUsize,
    state: Mutex<PoolState>,
}

impl ThreadPool {
    pub fn from(config: Config) -> Self {
        let max_threads = config.max_threads().unwrap().get();
        Self {
            config,
            pending: AtomicUsize::new(0),
            state: Mutex::new(PoolState {
                spawned: 0,
                idle: Vec::with_capacity(max_threads),
            }),
        }
    }

    pub fn spawn(&self, executor: &Arc<Executor>, worker_index: usize) -> Result<(), ()> {
        loop {
            let mut state = self.state.lock();
            if state.idle.len() > 0 {
                let parker = state.idle.swap_remove(0);
                drop(state);

                if parker.unpark(Some(worker_index)) {
                    return Ok(());
                } else {
                    continue;
                }
            }

            let max_threads = self.config.max_threads().unwrap().get();
            assert!(state.spawned <= max_threads);
            if state.spawned == max_threads {
                return Err(());
            }

            state.spawned += 1;
            break;
        }

        let executor = executor.clone();
        thread::Builder::new()
            .name(self.config.on_thread_name.as_ref().unwrap()())
            .stack_size(self.config.stack_size.unwrap().get())
            .spawn(move || executor.thread_pool.run(&executor, worker_index))
            .map(drop)
            .map_err(|_| self.finish())
    }

    fn run(&self, executor: &Arc<Executor>, worker_index: usize) {
        if let Some(on_thread_start) = self.config.on_thread_start.as_ref() {
            (on_thread_start)();
        }

        Self::run_with(executor, worker_index);
        self.finish();

        if let Some(on_thread_stop) = self.config.on_thread_stop.as_ref() {
            (on_thread_stop)();
        }
    }

    fn run_with(executor: &Arc<Executor>, worker_index: usize) {
        #[derive(Default)]
        struct ThreadPoolFuture {
            polled: bool,
        }

        impl Future for ThreadPoolFuture {
            type Output = ();

            fn poll(mut self: Pin<&mut Self>, _ctx: &mut PollContext<'_>) -> Poll<()> {
                if replace(&mut self.polled, true) {
                    Poll::Ready(())
                } else {
                    Poll::Pending
                }
            }
        }

        WorkerContext::block_on(executor, Some(worker_index), ThreadPoolFuture::default());
    }

    fn finish(&self) {
        let mut state = self.state.lock();

        let max_threads = self.config.max_threads().unwrap().get();
        assert!(state.spawned <= max_threads);

        assert_ne!(state.spawned, 0);
        state.spawned -= 1;
    }

    pub fn wait(&self, parker: &Arc<Parker>, timeout: Option<Duration>) -> Option<usize> {
        if let Some(worker_index) = parker.poll() {
            return worker_index;
        }

        let mut state = self.state.lock();
        if let Some(worker_index) = parker.poll() {
            return worker_index;
        }

        state.idle.push(parker.clone());
        drop(state);

        if let Some(on_thread_park) = self.config.on_thread_park.as_ref() {
            (on_thread_park)();
        }

        let worker_index = parker.poll().unwrap_or_else(|| {
            // slow path: actually block on the parker
            parker.park(timeout)
        });

        if let Some(on_thread_unpark) = self.config.on_thread_unpark.as_ref() {
            (on_thread_unpark)();
        }

        if worker_index.is_none() {
            let mut state = self.state.lock();
            for index in 0..state.idle.len() {
                if Arc::ptr_eq(&state.idle[index], parker) {
                    drop(state.idle.swap_remove(index));
                    break;
                }
            }
        }

        worker_index
    }

    pub fn shutdown(&self) {
        let mut state = self.state.lock();
        let idle = replace(&mut state.idle, Vec::new());
        drop(state);

        for parker in idle.into_iter() {
            parker.unpark(None);
        }
    }

    pub fn task_begin(&self) {
        let pending = self.pending.fetch_add(1, Ordering::Relaxed);
        assert_ne!(pending, usize::MAX);
    }

    pub fn task_complete(&self) {
        let pending = self.pending.fetch_sub(1, Ordering::Release);
        assert_ne!(pending, 0);

        if pending == 0 {
            self.shutdown();
        }
    }
}
