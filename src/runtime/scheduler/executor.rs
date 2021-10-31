use super::{
    pool::{Notified, ThreadPool, ThreadPoolConfig},
    queue::{Injector, Queue, Task},
    random::RandomIterSource,
    thread::Thread,
};
use crate::io::driver::Driver as IoDriver;
use std::{
    num::NonZeroUsize,
    sync::atomic::{fence, AtomicUsize, Ordering},
    sync::Arc,
};

#[derive(Default)]
pub struct Worker {
    idle_next: AtomicUsize,
    pub run_queue: Queue,
}

pub struct Executor {
    idle: AtomicUsize,
    searching: AtomicUsize,
    pub io_driver: Arc<IoDriver>,
    pub injector: Injector,
    pub rand_iter_source: RandomIterSource,
    pub thread_pool: ThreadPool,
    pub workers: Box<[Worker]>,
}

impl Executor {
    pub fn new(worker_threads: NonZeroUsize, mut config: ThreadPoolConfig) -> Arc<Self> {
        config.max_threads = config
            .max_threads
            .or_else(|| NonZeroUsize::new(512))
            .and_then(|max_threads| NonZeroUsize::new(max_threads.get() + worker_threads.get()));

        let executor = Arc::new(Self {
            idle: AtomicUsize::new(0),
            searching: AtomicUsize::new(0),
            io_driver: Arc::new(IoDriver::default()),
            injector: Injector::default(),
            rand_iter_source: RandomIterSource::from(worker_threads),
            thread_pool: ThreadPool::from(config),
            workers: (0..worker_threads.get())
                .map(|_| Worker::default())
                .collect(),
        });

        for worker_index in (0..worker_threads.get()).rev() {
            executor.push_idle_worker(worker_index);
        }

        executor
    }

    pub fn schedule(self: &Arc<Self>, tasks: impl Iterator<Item = Task>, thread: Option<&Thread>) {
        let mut tasks = tasks.peekable();
        if tasks.peek().is_none() {
            return;
        }

        if let Some(thread) = thread {
            if thread.use_ready.get() {
                thread.ready.borrow_mut().extend(tasks);
                return;
            }

            if let Some(producer) = thread.producer.borrow().as_ref() {
                producer.push(tasks);
                self.notify();
                return;
            }
        }

        self.injector.inject(tasks);
        fence(Ordering::SeqCst);
        self.notify()
    }

    fn notify(self: &Arc<Self>) {
        if self.peek_idle_worker().is_none() {
            return;
        }

        let searching = self.searching.load(Ordering::Relaxed);
        assert!(searching <= self.workers.len());
        if searching > 0 {
            return;
        }

        if let Err(searching) =
            self.searching
                .compare_exchange(0, 1, Ordering::Acquire, Ordering::Relaxed)
        {
            assert!(searching <= self.workers.len());
            return;
        }

        if let Some(worker_index) = self.pop_idle_worker() {
            match self.thread_pool.notify(
                self,
                Notified {
                    worker_index,
                    searching: true,
                },
            ) {
                Some(_) => return,
                None => self.push_idle_worker(worker_index),
            }
        }

        let searching = self.searching.fetch_sub(1, Ordering::Relaxed);
        assert!(searching <= self.workers.len());
        assert_ne!(searching, 0);
    }

    pub fn search_begin(&self) -> bool {
        let searching = self.searching.load(Ordering::Relaxed);
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
        self.push_idle_worker(worker_index);

        was_searching && {
            let searching = self.searching.fetch_sub(1, Ordering::SeqCst);
            assert!(searching <= self.workers.len());
            assert_ne!(searching, 0);

            searching == 1 && {
                let has_pending = self.injector.pending()
                    || self
                        .workers
                        .iter()
                        .map(|worker| worker.run_queue.pending())
                        .filter(|pending| !pending)
                        .next()
                        .unwrap_or(false);

                has_pending
            }
        }
    }

    pub fn search_retry(&self) -> Option<usize> {
        self.pop_idle_worker().map(|worker_index| {
            let searching = self.searching.fetch_add(1, Ordering::Relaxed);
            assert!(searching < self.workers.len());
            worker_index
        })
    }

    pub fn shutdown(&self) {
        self.thread_pool.shutdown();
        self.io_driver.notify();
    }
}

#[derive(Copy, Clone, Eq, PartialEq)]
struct Idle {
    top_index: Option<usize>,
    aba: usize,
}

impl Idle {
    const BITS: u32 = usize::BITS / 2;
    const MASK: usize = (1 << Self::BITS) - 1;
}

impl Into<usize> for Idle {
    fn into(self) -> usize {
        let top_index = self.top_index.map(|i| i + 1).unwrap_or(0);
        assert!(top_index <= Self::MASK);

        let aba = self.aba & Self::MASK;
        (aba << Self::BITS) | top_index
    }
}

impl From<usize> for Idle {
    fn from(value: usize) -> Self {
        Self {
            top_index: match value & Self::MASK {
                0 => None,
                index => Some(index - 1),
            },
            aba: value >> Self::BITS,
        }
    }
}

impl Executor {
    fn push_idle_worker(&self, worker_index: usize) {
        let _ = self
            .idle
            .fetch_update(Ordering::Release, Ordering::Relaxed, |idle| {
                let mut idle = Idle::from(idle);
                self.workers[worker_index]
                    .idle_next
                    .store(idle.into(), Ordering::Relaxed);

                idle.top_index = Some(worker_index);
                idle.aba += 1;
                Some(idle.into())
            });
    }

    fn peek_idle_worker(&self) -> Option<usize> {
        let idle = self.idle.load(Ordering::Acquire);
        Idle::from(idle).top_index
    }

    fn pop_idle_worker(&self) -> Option<usize> {
        self.idle
            .fetch_update(Ordering::Acquire, Ordering::Acquire, |idle| {
                let mut idle = Idle::from(idle);
                let worker_index = idle.top_index?;

                let next = self.workers[worker_index].idle_next.load(Ordering::Relaxed);
                idle.top_index = Idle::from(next).top_index;
                Some(idle.into())
            })
            .ok()
            .and_then(|idle| Idle::from(idle).top_index)
    }
}
