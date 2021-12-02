use super::task::TaskRunnable;
use std::{
    collections::VecDeque,
    sync::atomic::{AtomicBool, Ordering},
    sync::{Arc, Mutex},
};

pub type Runnable = Arc<dyn TaskRunnable>;

pub enum Steal {
    Success(Runnable),
    Empty,
    Retry,
}

pub struct Worker {
    buffer: Arc<Buffer>,
}

impl Worker {
    pub fn new_lifo() -> Self {
        Self {
            buffer: Arc::new(Buffer::default()),
        }
    }

    pub fn push(&self, runnable: Runnable) {
        self.buffer.push(runnable);
    }

    pub fn pop(&self) -> Option<Runnable> {
        self.buffer.pop()
    }

    pub fn stealer(&self) -> Stealer {
        Stealer {
            buffer: self.buffer.clone(),
        }
    }
}

#[derive(Default)]
pub struct Stealer {
    buffer: Arc<Buffer>,
}

impl Stealer {
    pub fn steal(&self) -> Steal {
        self.buffer.steal()
    }

    pub fn steal_batch_and_pop(&self, worker: &Worker) -> Steal {
        self.buffer.steal_into(&worker.buffer)
    }
}

#[derive(Default)]
pub struct Injector {
    buffer: Buffer,
}

impl Injector {
    pub fn is_empty(&self) -> bool {
        self.buffer.is_empty()
    }

    pub fn push(&self, runnable: Runnable) {
        self.buffer.push(runnable);
    }

    pub fn steal(&self) -> Steal {
        self.buffer.steal()
    }

    pub fn steal_batch_and_pop(&self, worker: &Worker) -> Steal {
        self.buffer.steal_into(&worker.buffer)
    }
}

#[derive(Default)]
struct Buffer {
    pending: AtomicBool,
    deque: Mutex<VecDeque<Runnable>>,
}

impl Buffer {
    fn is_empty(&self) -> bool {
        !self.pending.load(Ordering::Acquire)
    }

    fn push(&self, runnable: Runnable) {
        let mut deque = self.deque.lock().unwrap();
        deque.push_back(runnable);
        self.pending.store(true, Ordering::Relaxed);
    }

    fn pop(&self) -> Option<Runnable> {
        if self.is_empty() {
            return None;
        }

        let mut deque = self.deque.lock().unwrap();
        deque.pop_back().map(|runnable| {
            self.pending.store(deque.len() > 0, Ordering::Relaxed);
            runnable
        })
    }

    fn steal(&self) -> Steal {
        if self.is_empty() {
            return Steal::Empty;
        }

        let mut deque = match self.deque.try_lock() {
            Ok(guard) => guard,
            Err(_) => return Steal::Retry,
        };

        let runnable = match deque.pop_front() {
            Some(item) => item,
            None => return Steal::Empty,
        };

        self.pending.store(deque.len() > 0, Ordering::Relaxed);
        Steal::Success(runnable)
    }

    fn steal_into(&self, dst_buffer: &Self) -> Steal {
        if self.is_empty() {
            return Steal::Empty;
        }

        let mut deque = match self.deque.try_lock() {
            Ok(guard) => guard,
            Err(_) => return Steal::Retry,
        };

        let runnable = match deque.pop_front() {
            Some(item) => item,
            None => return Steal::Empty,
        };

        let batch_size = (deque.len() / 2).min(16);
        if batch_size > 0 {
            let mut dst_deque = dst_buffer.deque.lock().unwrap();
            dst_deque.extend(deque.drain(0..batch_size));
            dst_buffer.pending.store(true, Ordering::Relaxed);
        }

        self.pending.store(deque.len() > 0, Ordering::Relaxed);
        Steal::Success(runnable)
    }
}
