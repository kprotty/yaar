use super::{
    config::Config,
    context::Context,
    queue::{Injector, Runnable},
    thread_pool::ThreadPool,
    worker::Worker,
};
use std::{
    sync::atomic::{fence, AtomicBool, AtomicUsize, Ordering},
    sync::{Arc, Mutex},
};

pub struct Executor {
    idle: Idle,
    searching: AtomicUsize,
    pub injector: Injector,
    pub thread_pool: ThreadPool,
    pub workers: Box<[Worker]>,
}

impl From<Config> for Executor {
    fn from(config: Config) -> Self {
        let num_workers = config.worker_threads.get();

        Self {
            idle: Idle::from((0..num_workers).collect()),
            searching: AtomicUsize::new(0),
            injector: Injector::default(),
            thread_pool: ThreadPool::from(config),
            workers: (0..num_workers).map(|_| Worker::default()).collect(),
        }
    }
}

impl Executor {
    pub fn schedule(self: &Arc<Self>, context: Option<&Context>, runnable: Runnable) {
        if let Some(context) = context {
            if let Some(producer) = context.producer.borrow().as_ref() {
                producer.push(runnable);
                return self.notify();
            }
        }

        self.injector.push(runnable);
        fence(Ordering::SeqCst);
        self.notify();
    }

    fn notify(self: &Arc<Self>) {
        if !self.idle.pending.load(Ordering::Relaxed) {
            return;
        }

        let searching = self.searching.load(Ordering::Relaxed);
        assert!(searching <= self.workers.len());
        if searching > 0 {
            return;
        }

        if self
            .searching
            .compare_exchange(0, 1, Ordering::AcqRel, Ordering::Relaxed)
            .is_err()
        {
            return;
        }

        if let Some(worker_index) = self.idle.pop() {
            match self.thread_pool.spawn(self, worker_index) {
                Ok(_) => return,
                Err(_) => self.idle.push(worker_index),
            }
        }

        let searching = self.searching.fetch_sub(1, Ordering::Relaxed);
        assert!(searching <= self.workers.len());
        assert_ne!(searching, 0);
    }

    pub fn search_begin(&self) -> bool {
        let searching = self.searching.load(Ordering::Relaxed);
        assert!(searching <= self.workers.len());

        if (2 * searching) >= self.workers.len() {
            return false;
        }

        let searching = self.searching.fetch_add(1, Ordering::Acquire);
        assert!(searching < self.workers.len());
        true
    }

    pub fn search_discovered(&self) {
        let searching = self.searching.fetch_sub(1, Ordering::Relaxed);
        assert!(searching <= self.workers.len());
        assert_ne!(searching, 0);

        if searching == 1 {
            self.notify();
        }
    }

    pub fn search_failed(&self, was_searching: bool, worker_index: usize) -> Option<usize> {
        assert!(worker_index < self.workers.len());
        self.idle.push(worker_index);

        if was_searching {
            let searching = self.searching.fetch_sub(1, Ordering::SeqCst);
            assert!(searching <= self.workers.len());
            assert_ne!(searching, 0);

            if searching == 1 && self.injector.pending() {
                return self.search_retry();
            }
        }

        None
    }

    pub fn search_retry(&self) -> Option<usize> {
        self.idle.pop().map(|worker_index| {
            let searching = self.searching.fetch_add(1, Ordering::Acquire);
            assert!(searching < self.workers.len());
            worker_index
        })
    }

    pub fn call_blocking<F>(self: &Arc<Self>, f: impl FnOnce()) -> (F, Option<usize>) {
        // mark current worker as blocked
        // do f()
        // try to unmark current worker (races with monitor)
        // - on success return current worker index
        // - on failure, pop_idle_worker()
        // to inject on failure, wake_by_ref() with Pending
    }
}