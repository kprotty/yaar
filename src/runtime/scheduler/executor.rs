use super::{
    queue::{Injector, Runnable},
    random::RandomIterSource,
    thread::{Thread, ThreadPool},
    worker::Worker,
};
use parking_lot::Mutex;
use std::{
    collections::VecDeque,
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
    tasks: AtomicUsize,
    searching: AtomicUsize,
    pub thread_pool: ThreadPool,
    pub injector: Injector,
    pub rng_iter_source: RandomIterSource,
    pub workers: Box<[Worker]>,
}

impl Executor {
    pub fn new(worker_threads: Option<NonZeroUsize>) -> Self {
        let worker_threads = worker_threads
            .or_else(|| NonZeroUsize::new(num_cpus::get()))
            .or(NonZeroUsize::new(1))
            .unwrap();

        let executor = Self {
            idle: IdleQueue::from(worker_threads),
            tasks: AtomicUsize::new(0),
            searching: AtomicUsize::new(0),
            thread_pool: ThreadPool::new(worker_threads),
            rng_iter_source: RandomIterSource::from(worker_threads),
            injector: Injector::default(),
            workers: (0..worker_threads.get())
                .map(|_| Worker::default())
                .collect(),
        };

        executor
    }

    pub fn schedule(self: &Arc<Self>, runnable: Runnable, thread: Option<&Thread>, be_fair: bool) {
        if let Some(thread) = thread {
            if let Some(producer) = thread.producer.borrow().as_ref() {
                producer.push(runnable, be_fair);
                self.notify();
                return;
            }
        }

        self.injector.push(runnable);
        fence(Ordering::SeqCst);
        self.notify();
    }

    fn notify(self: &Arc<Self>) {
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

    pub fn task_begin(&self) {
        let tasks = self.tasks.fetch_add(1, Ordering::Relaxed);
        assert_ne!(tasks, usize::MAX);
    }

    pub fn task_complete(&self) {
        let tasks = self.tasks.fetch_sub(1, Ordering::Release);
        assert_ne!(tasks, 0);

        if tasks == 1 {
            self.thread_pool.shutdown();
        }
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
