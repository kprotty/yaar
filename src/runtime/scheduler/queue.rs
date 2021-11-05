use super::task::TaskRunnable;
use crossbeam_deque::{
    Injector as QueueInjector, Steal as QueueSteal, Stealer as QueueStealer, Worker as QueueWorker,
};
use parking_lot::Mutex;
use std::{
    cell::{Cell, RefCell},
    collections::VecDeque,
    hint::spin_loop,
    mem::{drop, replace, swap},
    sync::atomic::{AtomicBool, Ordering},
    sync::Arc,
};
use try_lock::TryLock;

pub type Runnable = Arc<dyn TaskRunnable>;

pub type Steal = QueueSteal<Runnable>;

pub struct Queue {
    stealer: QueueStealer<Runnable>,
    producer: TryLock<Option<Producer>>,
}

impl Default for Queue {
    fn default() -> Self {
        let producer = Producer::default();
        let stealer = producer.stealer.clone();

        Self {
            stealer,
            producer: TryLock::new(Some(producer)),
        }
    }
}

impl Queue {
    pub fn swap_producer(&self, new_producer: Option<Producer>) -> Option<Producer> {
        let mut producer = self.producer.try_lock().unwrap();
        replace(&mut *producer, new_producer)
    }
}

#[derive(Default)]
pub struct Injector {
    injector: QueueInjector<Runnable>,
    batch_pending: AtomicBool,
    batch: Mutex<VecDeque<Runnable>>,
}

impl Injector {
    pub fn push(&self, runnable: Runnable) {
        self.injector.push(runnable);
    }

    pub fn inject(&self, runnables: impl Iterator<Item = Runnable>) {
        let mut batch = self.batch.lock();
        batch.extend(runnables);
        self.batch_pending.store(batch.len() > 0, Ordering::Relaxed);
    }

    pub fn pending(&self) -> bool {
        !self.injector.is_empty() || self.batch_pending.load(Ordering::SeqCst)
    }
}

pub struct Producer {
    be_fair: Cell<bool>,
    batch: RefCell<VecDeque<Runnable>>,
    worker: QueueWorker<Runnable>,
    stealer: QueueStealer<Runnable>,
}

impl Default for Producer {
    fn default() -> Self {
        let worker = QueueWorker::new_lifo();
        let stealer = worker.stealer();

        Self {
            be_fair: Cell::new(false),
            batch: RefCell::new(VecDeque::new()),
            worker,
            stealer,
        }
    }
}

impl Producer {
    pub fn push(&self, runnable: Runnable, be_fair: bool) {
        self.worker.push(runnable);
        if be_fair {
            self.be_fair.set(true);
        }
    }

    pub fn pop(&self, be_fair: bool) -> Option<Runnable> {
        let be_fair = be_fair || self.be_fair.replace(false);
        if !be_fair {
            return self.worker.pop();
        }

        loop {
            match self.stealer.steal() {
                Steal::Success(runnable) => return Some(runnable),
                Steal::Retry => spin_loop(),
                Steal::Empty => return None,
            }
        }
    }

    pub fn steal(&self, queue: &Queue) -> Steal {
        queue.stealer.steal_batch_and_pop(&self.worker)
    }

    pub fn consume(&self, injector: &Injector) -> Steal {
        let result = match injector.injector.steal_batch_and_pop(&self.worker) {
            Steal::Success(runnable) => return Steal::Success(runnable),
            s => s,
        };

        if !injector.batch_pending.load(Ordering::SeqCst) {
            return result;
        }

        let mut injector_batch = match injector.batch.try_lock() {
            Some(guard) => guard,
            None => return Steal::Retry,
        };

        if !injector.batch_pending.load(Ordering::Relaxed) {
            return result;
        }

        let mut batch = self.batch.borrow_mut();
        assert_eq!(batch.len(), 0);

        swap(&mut *batch, &mut *injector_batch);
        injector.batch_pending.store(false, Ordering::Relaxed);
        drop(injector_batch);

        let runnable = batch.pop_front();
        batch
            .drain(..)
            .for_each(|runnable| self.worker.push(runnable));

        match runnable {
            Some(runnable) => Steal::Success(runnable),
            None => result,
        }
    }
}
