use super::{
    config::Config,
    context::Context,
    worker::Worker,
    thread_pool::ThreadPool,
    queue::{Injector, Runnable},
};
use std::{
    sync::{Arc, Mutex},
    sync::atomic::{fence, AtomicUsize, AtomicBool, Ordering},
};

struct Idle {
    pending: AtomicBool,
    indices: Mutex<Vec<usize>>,
}

impl From<Vec<usize>> for Idle {
    fn from(indices: Vec<usize>) -> Self {
        Self {
            pending: AtomicBool::new(indices.len() > 0),
            indices: Mutex::new(indices),
        }
    }
}

impl Idle {
    fn push(&self, index: usize) {
        let mut indices = self.indices.lock().unwrap();
        self.pending.store(true, Ordering::Relaxed);
        indices.push(index);
    }

    fn pop(&self) -> Option<usize> {
        if !self.pending.load(Ordering::Acquire) {
            return None;
        }

        let mut indices = self.indices.lock().unwrap();
        let index = indices.len().checked_sub(1);
        let index = index.map(|i| indices.swap_remove(i));

        self.pending.store(indices.len() > 0, Ordering::Relaxed);
        index
    }
}

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

        if self.searching
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

    pub(super) fn search_begin(&self) -> bool {
        let searching = self.searching.load(Ordering::Relaxed);
        assert!(searching <= self.workers.len());

        if (2 * searching) >= self.workers.len() {
            return false;
        }

        let searching = self.searching.fetch_add(1, Ordering::Acquire);
        assert!(searching < self.workers.len());
        true
    }

    pub(super) fn search_discovered(&self) {
        let searching = self.searching.fetch_sub(1, Ordering::Relaxed);
        assert!(searching <= self.workers.len());
        assert_ne!(searching, 0);

        if searching == 1 {
            self.notify();
        }
    }

    pub(super) fn search_failed(&self, was_searching: bool, worker_index: usize) -> Option<usize> {
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

    pub(super) fn search_retry(&self) -> Option<usize> {
        self.idle.pop().map(|worker_index| {
            let searching = self.searching.fetch_add(1, Ordering::Acquire);
            assert!(searching < self.workers.len());
            worker_index
        })
    }
}