use super::{
    pool::{Config as ThreadPoolConfig, Notified, ThreadPool},
    queue::{Injector, Queue, Runnable},
    random::RandomIterGen,
    thread::Thread,
};
use crate::io::Driver as IoDriver;
use std::{
    io,
    sync::atomic::{fence, AtomicUsize, Ordering},
    sync::Arc,
};

#[derive(Default)]
pub struct Worker {
    pub run_queue: Queue,
    idle_next: AtomicUsize,
}

pub struct Executor {
    idle: AtomicUsize,
    searching: AtomicUsize,
    pub injector: Injector,
    pub io_driver: Arc<IoDriver>,
    pub thread_pool: ThreadPool,
    pub rng_iter_gen: RandomIterGen,
    pub workers: Box<[Worker]>,
}

impl Executor {
    pub fn new(config: ThreadPoolConfig) -> io::Result<Self> {
        let io_driver = IoDriver::new()?;
        let worker_threads = config.worker_threads.unwrap();

        let executor = Self {
            idle: AtomicUsize::new(0),
            searching: AtomicUsize::new(0),
            injector: Injector::default(),
            io_driver: Arc::new(io_driver),
            thread_pool: ThreadPool::from(config),
            rng_iter_gen: RandomIterGen::from(worker_threads),
            workers: (0..worker_threads.get())
                .map(|_| Worker::default())
                .collect(),
        };

        for worker_index in (0..worker_threads.get()).rev() {
            executor.push_idle_worker(worker_index);
        }

        Ok(executor)
    }

    pub fn schedule(
        self: &Arc<Self>,
        runnables: impl Iterator<Item = Runnable>,
        thread: Option<&Thread>,
    ) {
        let mut runnables = runnables.peekable();
        if runnables.peek().is_none() {
            return;
        }

        if let Some(thread) = thread {
            if let Some(ref mut queue) = &mut *thread.io_intercept.borrow_mut() {
                queue.extend(runnables);
                return;
            }

            if let Some(ref producer) = thread.producer.borrow().as_ref() {
                producer.push(runnables);
                self.notify();
                return;
            }
        }

        self.injector.push(runnables);
        fence(Ordering::SeqCst);
        self.notify();
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
                .compare_exchange(0, 1, Ordering::SeqCst, Ordering::Relaxed)
        {
            assert!(searching <= self.workers.len());
            return;
        }

        if let Some(worker_index) = self.pop_idle_worker() {
            let notified = Notified {
                worker_index,
                searching: true,
            };

            match self.thread_pool.notify(self, notified) {
                Ok(_) => return,
                Err(_) => self.push_idle_worker(worker_index),
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

    pub fn search_discovered(self: &Arc<Self>) {
        let searching = self.searching.fetch_sub(1, Ordering::SeqCst);
        assert!(searching <= self.workers.len());
        assert_ne!(searching, 0);

        if searching == 1 {
            self.notify();
        }
    }

    pub fn search_failed(&self, worker_index: usize, was_searching: bool) -> bool {
        assert!(worker_index < self.workers.len());
        self.push_idle_worker(worker_index);

        was_searching && {
            let searching = self.searching.fetch_sub(1, Ordering::SeqCst);
            assert!(searching <= self.workers.len());
            assert_ne!(searching, 0);
            searching == 1 && self.pending()
        }
    }

    pub fn search_retry(&self) -> Option<Notified> {
        self.pop_idle_worker().map(|worker_index| Notified {
            worker_index,
            searching: false,
        })
    }

    fn pending(&self) -> bool {
        if self.injector.pending() {
            return true;
        }

        self.workers
            .iter()
            .map(|worker| worker.run_queue.pending())
            .filter(|pending| !pending)
            .next()
            .unwrap_or(false)
    }
}

const IDLE_BITS: u32 = usize::BITS / 2;
const IDLE_MASK: usize = (1 << IDLE_BITS) - 1;

impl Executor {
    fn push_idle_worker(&self, worker_index: usize) {
        let _ = self
            .idle
            .fetch_update(Ordering::Release, Ordering::Relaxed, |mut idle| {
                let top_index = idle & IDLE_MASK;
                self.workers[worker_index]
                    .idle_next
                    .store(top_index, Ordering::Relaxed);

                let aba_count = idle >> IDLE_BITS;
                idle = ((aba_count + 1) << IDLE_BITS) | (worker_index + 1);
                Some(idle)
            });
    }

    fn peek_idle_worker(&self) -> Option<usize> {
        let idle = self.idle.load(Ordering::Acquire);
        match idle & IDLE_MASK {
            0 => None,
            top_index => Some(top_index - 1),
        }
    }

    fn pop_idle_worker(&self) -> Option<usize> {
        self.idle
            .fetch_update(Ordering::AcqRel, Ordering::Acquire, |mut idle| {
                let worker_index = match idle & IDLE_MASK {
                    0 => return None,
                    top_index => top_index - 1,
                };

                let top_index = self.workers[worker_index].idle_next.load(Ordering::Relaxed);
                idle = (idle & !IDLE_MASK) | top_index;
                Some(idle)
            })
            .map(|idle| (idle & IDLE_MASK) - 1)
            .ok()
    }
}
