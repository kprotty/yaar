use super::task::TaskRunnable;
use std::{mem::replace, sync::Arc};
use try_lock::TryLock;

pub type Runnable = Arc<dyn TaskRunnable>;

pub type Steal = crossbeam_deque::Steal<Runnable>;

#[derive(Default)]
pub struct Injector {
    inner: crossbeam_deque::Injector<Runnable>
}

impl Injector {
    pub fn push(&self, runnable: Runnable) {
        self.inner.push(runnable);
    }

    pub fn pending(&self) -> bool {
        !self.inner.is_empty()
    }
}

pub struct Queue {
    inner: crossbeam_deque::Stealer<Runnable>,
    producer: TryLock<Option<Producer>>,
}

impl Default for Queue {
    fn default() -> Self {
        let worker = crossbeam_deque::Worker::new_lifo();
        let stealer = worker.stealer();
        Self {
            inner: stealer,
            producer: TryLock::new(Some(Producer { inner: worker })),
        }
    }
}

impl Queue {
    pub fn swap_producer(&self, old_value: Option<Producer>) -> Option<Producer> {
        replace(&mut *self.producer.try_lock(), old_value)
    }
}

pub struct Producer {
    inner: crossbeam_deque::Worker<Runnable>,
}

impl Producer {
    pub fn push(&self, runnable: Runnable) {
        self.inner.push(runnable);
    }

    pub fn pop(&self) -> Option<Runnable> {
        self.inner.pop();
    }

    pub fn steal(&self, queue: &Queue) -> Steal {
        queue.inner.steal_batch_and_pop(&self.inner)
    }

    pub fn consume(&self, injector: &Injector) -> Steal {
        injector.inner.steal_batch_and_pop(&self.inner)
    }
}
