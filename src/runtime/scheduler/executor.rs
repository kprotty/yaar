use super::{
    config::Config,
    context::Context,
    pool::ThreadPool,
    queue::{Injector, Runnable},
    random::RandomIterSource,
    worker::Worker,
};
use crate::io::driver::Driver as IoDriver;
use parking_lot::Mutex;
use std::{
    collections::VecDeque,
    io, iter,
    num::NonZeroUsize,
    sync::atomic::{fence, AtomicBool, AtomicUsize, Ordering},
    sync::Arc,
};

struct IdleQueue {
    pending: AtomicBool,
    indices: Mutex<VecDeque<usize>>,
}

impl From<NonZeroUsize> for IdleQueue {
    fn from(count: NonZeroUsize) -> Self {
        Self {
            pending: AtomicBool::new(true),
            indices: Mutex::new((0..count.get()).collect()),
        }
    }
}

impl IdleQueue {
    fn pending(&self) -> bool {
        self.pending.load(Ordering::SeqCst)
    }

    fn push(&self, index: usize) {
        let mut indices = self.indices.lock();
        indices.push_back(index);
        self.pending.store(true, Ordering::Relaxed);
    }

    fn pop(&self) -> Option<usize> {
        if !self.pending() {
            return None;
        }

        let mut indices = self.indices.lock();
        indices.pop_back().map(|index| {
            self.pending.store(indices.len() > 0, Ordering::Relaxed);
            index
        })
    }
}

pub struct Executor {
    idle: IdleQueue,
    searching: AtomicUsize,
    pub io_driver: Arc<IoDriver>,
    pub thread_pool: ThreadPool,
    pub injector: Injector,
    pub rng_iter_source: RandomIterSource,
    pub workers: Box<[Worker]>,
}

impl Executor {
    pub fn from(config: Config) -> io::Result<Self> {
        let io_driver = IoDriver::new()?;
        let worker_threads = config.worker_threads.unwrap();

        Ok(Self {
            idle: IdleQueue::from(worker_threads),
            searching: AtomicUsize::new(0),
            io_driver: Arc::new(io_driver),
            thread_pool: ThreadPool::from(config),
            rng_iter_source: RandomIterSource::from(worker_threads),
            injector: Injector::default(),
            workers: (0..worker_threads.get())
                .map(|_| Worker::default())
                .collect(),
        })
    }

    pub fn schedule(
        self: &Arc<Self>,
        runnable: Runnable,
        context: Option<&Context>,
        be_fair: bool,
    ) {
        if let Some(context) = context {
            assert!(Arc::ptr_eq(self, &context.executor));

            if let Some(intercept) = context.intercept.borrow_mut().as_mut() {
                intercept.push_back(runnable);
                return;
            }

            if let Some(producer) = context.producer.borrow().as_ref() {
                producer.push(runnable, be_fair);
                self.notify();
                return;
            }
        }

        self.inject(iter::once(runnable))
    }

    pub fn inject(self: &Arc<Self>, runnables: impl Iterator<Item = Runnable>) {
        runnables.for_each(|runnable| self.injector.push(runnable));
        fence(Ordering::SeqCst);
        self.notify();
    }

    pub fn notify(self: &Arc<Self>) {
        if !self.idle.pending() {
            return;
        }

        let searching = self.searching.load(Ordering::Relaxed);
        assert!(searching <= self.workers.len());
        if searching > 0 {
            return;
        }

        if let Err(searching) =
            self.searching
                .compare_exchange(0, 1, Ordering::SeqCst, Ordering::Relaxed)
        {
            assert!(searching <= self.workers.len());
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

        let searching = self.searching.fetch_add(1, Ordering::Relaxed);
        assert!(searching < self.workers.len());
        true
    }

    pub fn search_discovered(self: &Arc<Self>) {
        let searching = self.searching.fetch_sub(1, Ordering::SeqCst);
        assert!(searching <= self.workers.len());
        assert_ne!(searching, 0);

        if searching == 1 {
            self.notify();
        }
    }

    pub fn search_failed(&self, worker_index: usize, was_searching: bool) -> bool {
        assert!(worker_index <= self.workers.len());
        self.idle.push(worker_index);

        was_searching && {
            let searching = self.searching.fetch_sub(1, Ordering::SeqCst);
            assert!(searching <= self.workers.len());
            assert_ne!(searching, 0);

            searching == 1 && self.injector.pending()
        }
    }

    pub fn search_retry(&self) -> Option<usize> {
        self.idle.pop().map(|worker_index| {
            let searching = self.searching.fetch_add(1, Ordering::Relaxed);
            assert!(searching < self.workers.len());
            worker_index
        })
    }
}
