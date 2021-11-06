use super::task::TaskRunnable;
use crossbeam_deque::{
    Injector as QueueInjector, 
    Steal as QueueSteal, 
    Stealer as QueueStealer, 
    Worker as QueueWorker
};
use std::{cell::Cell, hint::spin_loop, mem::replace, sync::Arc};
use try_lock::TryLock;

pub type Runnable = Arc<dyn TaskRunnable>;
pub type Steal = QueueSteal<Runnable>;

#[derive(Default)]
pub struct Injector {
    injector: QueueInjector<Runnable>,
}

impl Injector {
    pub fn push(&self, runnable: Runnable) {
        self.injector.push(runnable);
    }

    pub fn pending(&self) -> bool {
        !self.injector.is_empty()
    }
}


pub struct Queue {
    stealer: QueueStealer<Runnable>,
    producer: TryLock<Option<Producer>>,
}

impl Default for Queue {
    fn default() -> Self {
        let producer = Producer::new();
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

pub struct Producer {
    be_fair: Cell<bool>,
    worker: QueueWorker<Runnable>,
    stealer: QueueStealer<Runnable>,
}

impl Producer {
    fn new() -> Self {
        let worker = QueueWorker::new_lifo();
        let stealer = worker.stealer();

        Self {
            be_fair: Cell::new(false),
            worker,
            stealer,
        }
    }

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
        injector.injector.steal_batch_and_pop(&self.worker)
    }
}
