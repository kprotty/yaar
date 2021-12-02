use super::{
    context::Context,
    idle::IdleQueue,
    parker::Parker,
    queue::{Injector as QueueInjector, Runnable},
    random::RngSeqSeed,
    worker::Worker,
};
use crate::dependencies::parking_lot::Mutex;
use once_cell::sync::OnceCell;
use std::{
    future,
    mem::drop,
    num::NonZeroUsize,
    pin::Pin,
    sync::atomic::{fence, AtomicUsize, Ordering},
    sync::Arc,
    thread,
};

pub struct Executor {
    idle_queue: IdleQueue,
    searching: AtomicUsize,
    parked: Mutex<Vec<Arc<Parker>>>,
    pub rng_seq_seed: RngSeqSeed,
    pub injector: QueueInjector,
    pub workers: Box<[Worker]>,
}

impl Executor {
    pub fn global() -> &'static Self {
        static GLOBAL: OnceCell<Executor> = OnceCell::new();
        GLOBAL.get_or_init(|| {
            let max_threads = NonZeroUsize::new(num_cpus::get())
                .or(NonZeroUsize::new(1))
                .unwrap();

            let executor = Self {
                idle_queue: IdleQueue::default(),
                searching: AtomicUsize::new(0),
                parked: Mutex::new(Vec::with_capacity(max_threads.get())),
                rng_seq_seed: RngSeqSeed::new(max_threads),
                injector: QueueInjector::new(),
                workers: (0..max_threads.get()).map(|_| Worker::new()).collect(),
            };

            for worker_index in (0..executor.workers.len()).rev() {
                executor.idle_push(worker_index);
            }

            executor
        })
    }

    fn idle_empty(&self) -> bool {
        self.idle_queue.is_empty()
    }

    fn idle_push(&self, worker_index: usize) {
        self.idle_queue.push(worker_index, |next_index| {
            self.workers[worker_index]
                .idle_next
                .store(next_index, Ordering::Relaxed);
        })
    }

    fn idle_pop(&self) -> Option<usize> {
        self.idle_queue
            .pop(|worker_index| self.workers[worker_index].idle_next.load(Ordering::Relaxed))
    }

    pub fn schedule(&self, runnable: Runnable, context: Option<&Context>) {
        if let Some(context) = context {
            if let Some(queue_worker) = context.queue_worker.borrow().as_ref() {
                queue_worker.push(runnable);
                self.notify();
                return;
            }
        }

        self.injector.push(runnable);
        fence(Ordering::SeqCst);
        self.notify()
    }

    fn notify(&self) {
        if self.idle_empty() {
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

        if let Some(worker_index) = self.idle_pop() {
            match self.unpark(Some(worker_index)) {
                Ok(_) => return,
                Err(_) => self.idle_push(worker_index),
            }
        }

        let searching = self.searching.fetch_sub(1, Ordering::Relaxed);
        assert!(searching <= self.workers.len());
        assert_ne!(searching, 0);
    }

    pub fn park(&self, parker: &Arc<Parker>) -> Option<usize> {
        if let Some(worker_index) = parker.poll() {
            return worker_index;
        }

        let mut parked = self.parked.lock();
        if let Some(worker_index) = parker.poll() {
            return worker_index;
        }

        parked.push(parker.clone());
        drop(parked);

        if let Some(worker_index) = parker.park() {
            return Some(worker_index);
        }

        let mut parked = self.parked.lock();
        for index in 0..parked.len() {
            if Arc::ptr_eq(&parked[index], parker) {
                drop(parked.swap_remove(index));
                break;
            }
        }

        drop(parked);
        parker.poll().unwrap()
    }

    fn unpark(&self, worker_index: Option<usize>) -> Result<(), ()> {
        {
            let mut parked = self.parked.lock();
            while let Some(index) = parked.len().checked_sub(1) {
                let parker = parked.swap_remove(index);
                drop(parked);

                if parker.unpark(worker_index) {
                    return Ok(());
                } else {
                    parked = self.parked.lock();
                }
            }
        }

        thread::Builder::new()
            .spawn(move || {
                let mut future = future::pending::<()>();
                Worker::block_on(worker_index, Pin::new(&mut future))
            })
            .map(drop)
            .map_err(drop)
    }

    pub fn search_begin(&self) -> bool {
        let searching = self.searching.load(Ordering::Relaxed);
        if (2 * searching) >= self.workers.len() {
            return false;
        }

        let searching = self.searching.fetch_add(1, Ordering::Acquire);
        assert!(searching < self.workers.len());
        true
    }

    pub fn search_discovered(&self) {
        let searching = self.searching.fetch_sub(1, Ordering::Release);
        assert!(searching <= self.workers.len());
        assert_ne!(searching, 0);

        if searching == 1 {
            self.notify();
        }
    }

    pub fn search_failed(&self, was_searching: bool, worker_index: usize) -> bool {
        assert!(worker_index <= self.workers.len());
        self.idle_push(worker_index);

        was_searching && {
            let searching = self.searching.fetch_sub(1, Ordering::SeqCst);
            assert!(searching <= self.workers.len());
            assert_ne!(searching, 0);

            searching == 1 && !self.injector.is_empty()
        }
    }

    pub fn search_retry(&self) -> Option<usize> {
        self.idle_pop().map(|worker_index| {
            let searching = self.searching.fetch_add(1, Ordering::Acquire);
            assert!(searching < self.workers.len());
            worker_index
        })
    }
}
