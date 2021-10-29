use super::task::TaskRunnable;
use std::sync::Arc;
use parking_lot::Mutex;

pub type Task = Arc<dyn TaskRunnable>;

#[derive(Copy, Clone, Eq, PartialEq)]
pub enum PopError {
    Empty,
    Contended,
}

#[derive(Default)]
pub struct Injector {
    inner: crossbeam::deque::Injector<Task>,
}

impl Injector {
    pub fn pending(&self) -> bool {
        !self.inner.is_empty()
    }

    pub fn inject(&self, task: Task) {
        self.inner.push(task);
    }
}

pub struct Producer {
    worker: crossbeam::deque::Worker<Task>,
}

impl Default for Producer {
    fn default() -> Self {
        Self {
            worker: crossbeam::deque::Worker::new_lifo(),
        }
    }
}

impl Producer {
    pub fn push(&self, task: Task) {
        self.worker.push(task);
    }

    pub fn pop(&self) -> Option<Task> {
        self.worker.pop()
    }

    pub fn steal(&self, queue: &Queue) -> Result<Task, PopError> {
        Self::stole(queue.stealer.steal_batch_and_pop(&self.worker))
    }

    pub fn consume(&self, injector: &Injector) -> Result<Task, PopError> {
        Self::stole(injector.inner.steal_batch_and_pop(&self.worker))
    }

    fn stole(result: crossbeam::deque::Steal<Task>) -> Result<Task, PopError> {
        match result {
            crossbeam::deque::Steal::Success(task) => Ok(task),
            crossbeam::deque::Steal::Empty => Err(PopError::Empty),
            crossbeam::deque::Steal::Retry => Err(PopError::Contended),
        }
    }
}

pub struct Queue {
    producer: Mutex<Option<Producer>>,
    stealer: crossbeam::deque::Stealer<Task>,
}

impl Default for Queue {
    fn default() -> Self {
        let producer = Producer::default();
        let stealer = producer.worker.stealer();
        Self {
            producer: Mutex::new(Some(producer)),
            stealer,
        }
    }
}

impl Queue {
    pub fn pending(&self) -> bool {
        !self.stealer.is_empty()
    }

    pub fn swap_producer(&self, old: Option<Producer>) -> Option<Producer> {
        std::mem::replace(&mut *self.producer.lock(), old)
    }
}
