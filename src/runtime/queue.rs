use super::task::TaskPoll;
use parking_lot::Mutex;
use std::{
    any::Any,
    collections::VecDeque,
    mem,
    sync::atomic::{AtomicBool, Ordering},
    sync::Arc,
    task::Waker,
};

#[derive(Copy, Clone, Eq, PartialEq)]
pub enum Error {
    Empty,
    Contended,
}

pub type Task = Arc<dyn TaskPoll>;

pub struct Queue {
    pending: AtomicBool,
    producer: Mutex<VecDeque<Task>>,
    consumer: Mutex<VecDeque<Task>>,
}

impl Default for Queue {
    fn default() -> Self {
        Self {
            pending: AtomicBool::new(false),
            producer: Mutex::new(VecDeque::new()),
            consumer: Mutex::new(VecDeque::new()),
        }
    }
}

impl Queue {
    pub fn push(&self, node: Task) {
        let mut producer = self.producer.lock();
        producer.push_back(node);
        self.pending.store(true, Ordering::Relaxed);
    }

    pub fn pending(&self) -> bool {
        self.pending.load(Ordering::Acquire)
    }

    pub fn pop(&self) -> Result<Task, Error> {
        if !self.pending.load(Ordering::Relaxed) {
            return Err(Error::Empty);
        }

        let mut consumer = match self.consumer.try_lock() {
            Some(guard) => guard,
            _ => return Err(Error::Contended),
        };

        if let Some(node) = consumer.pop_front() {
            return Ok(node);
        }

        if !self.pending.load(Ordering::Relaxed) {
            return Err(Error::Empty);
        }

        match self.producer.try_lock() {
            Some(mut producer) => {
                mem::swap(&mut *consumer, &mut *producer);
                self.pending.store(consumer.len() > 0, Ordering::Relaxed);
            }
            _ => return Err(Error::Contended),
        }

        consumer.pop_front().ok_or(Error::Empty)
    }
}
