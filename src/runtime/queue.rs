use super::task::TaskRunnable;
use parking_lot::Mutex;
use std::{
    collections::VecDeque,
    mem,
    sync::atomic::{AtomicBool, Ordering},
    sync::Arc,
};
use try_lock::TryLock;

#[derive(Copy, Clone, Eq, PartialEq)]
pub enum PopError {
    Empty,
    Contended,
}

pub type Task = Arc<dyn TaskRunnable>;

pub struct Queue {
    pending: AtomicBool,
    producer: Mutex<VecDeque<Task>>,
    consumer: TryLock<VecDeque<Task>>,
}

impl Default for Queue {
    fn default() -> Self {
        Self {
            pending: AtomicBool::new(false),
            producer: Mutex::new(VecDeque::new()),
            consumer: TryLock::new(VecDeque::new()),
        }
    }
}

impl Queue {
    pub fn inject(&self, tasks: impl Iterator<Item = Task>) {
        self.push(tasks)
    }

    pub fn push(&self, tasks: impl Iterator<Item = Task>) {
        let mut producer = self.producer.lock();
        producer.extend(tasks);

        if !self.pending.load(Ordering::Relaxed) && producer.len() > 0 {
            self.pending.store(true, Ordering::Relaxed);
        }
    }

    pub fn pending(&self) -> bool {
        self.pending.load(Ordering::Acquire)
    }

    pub fn pop(&self) -> Option<Task> {
        self.take().ok()
    }

    pub fn steal(&self, target: &Self) -> Result<Task, PopError> {
        target.take()
    }

    fn take(&self) -> Result<Task, PopError> {
        if !self.pending.load(Ordering::Relaxed) {
            return Err(PopError::Empty);
        }

        let mut consumer = match self.consumer.try_lock() {
            Some(guard) => guard,
            None => return Err(PopError::Contended),
        };

        if !self.pending.load(Ordering::Relaxed) {
            return Err(PopError::Empty);
        }

        if let Some(task) = consumer.pop_front() {
            return Ok(task);
        }

        match self.producer.try_lock() {
            Some(mut producer) => {
                mem::swap(&mut *producer, &mut *consumer);
                if consumer.len() == 0 {
                    self.pending.store(false, Ordering::Relaxed);
                }
            }
            None => return Err(PopError::Contended),
        }

        consumer.pop_front().ok_or(PopError::Contended)
    }
}
