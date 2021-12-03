use super::task::TaskRunnable;
use std::mem::replace;
use std::sync::Arc;
use try_lock::TryLock;
use crate::dependencies::crossbeam_deque;

pub type Runnable = Arc<dyn TaskRunnable>;

pub type Steal = crossbeam_deque::Steal<Runnable>;
pub type Injector = crossbeam_deque::Injector<Runnable>;

pub struct Queue {
    producer: TryLock<Option<Producer>>,
    stealer: crossbeam_deque::Stealer<Runnable>,
}

impl Default for Queue {
    fn default() -> Self {
        let worker = crossbeam_deque::Worker::new_lifo();
        let stealer = worker.stealer();
        Self {
            producer: TryLock::new(Some(Producer { worker })),
            stealer,
        }
    }
}

impl Queue {
    pub fn swap_producer(&self, old: Option<Producer>) -> Option<Producer> {
        let mut producer = self
            .producer
            .try_lock()
            .expect("multiple threads accessing a Queue's producer");
        replace(&mut *producer, old)
    }
}

pub struct Producer {
    worker: crossbeam_deque::Worker<Runnable>,
}

impl Producer {
    pub fn push(&self, runnable: Runnable) {
        self.worker.push(runnable)
    }

    pub fn pop(&self) -> Option<Runnable> {
        self.worker.pop()
    }

    pub fn steal(&self, queue: &Queue) {
        queue.stealer.steal_batch_and_pop(&self.worker)
    }

    pub fn consume(&self, injector: &Injector) {
        injector.steal_batch_and_pop(&self.worker)
    }
}


