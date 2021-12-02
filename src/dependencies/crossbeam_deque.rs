use super::parking_lot::Mutex;
use std::{
    cell::Cell,
    collections::VecDeque,
    sync::atomic::{AtomicBool, Ordering},
    sync::Arc,
};

pub enum Steal<T> {
    Success(T),
    Empty,
    Retry,
}

pub struct Worker<T> {
    buffer: Arc<Buffer<T>>,
    _non_send: Cell<()>,
}

impl<T> Worker<T> {
    pub fn new_lifo() -> Self {
        Self {
            buffer: Arc::new(Buffer::new()),
            _non_send: Cell::new(()),
        }
    }

    pub fn push(&self, item: T) {
        self.buffer.push(item);
    }

    pub fn pop(&self) -> Option<T> {
        self.buffer.pop()
    }

    pub fn stealer(&self) -> Stealer<T> {
        Stealer {
            buffer: self.buffer.clone(),
        }
    }
}

pub struct Stealer<T> {
    buffer: Arc<Buffer<T>>,
}

impl<T> Stealer<T> {
    pub fn steal(&self) -> Steal<T> {
        self.buffer.steal()
    }

    pub fn steal_batch_and_pop(&self, worker: &Worker<T>) -> Steal<T> {
        self.buffer.steal_into(&worker.buffer)
    }
}

pub struct Injector<T> {
    buffer: Buffer<T>,
}

impl<T> Injector<T> {
    pub fn new() -> Self {
        Self {
            buffer: Buffer::new(),
        }
    }

    pub fn is_empty(&self) -> bool {
        self.buffer.is_empty()
    }

    pub fn push(&self, item: T) {
        self.buffer.push(item);
    }

    pub fn steal(&self) -> Steal<T> {
        self.buffer.steal()
    }

    pub fn steal_batch_and_pop(&self, worker: &Worker<T>) -> Steal<T> {
        self.buffer.steal_into(&worker.buffer)
    }
}

struct Buffer<T> {
    pending: AtomicBool,
    deque: Mutex<VecDeque<T>>,
}

impl<T> Buffer<T> {
    fn new() -> Self {
        Self {
            pending: AtomicBool::new(false),
            deque: Mutex::new(VecDeque::new()),
        }
    }

    fn is_empty(&self) -> bool {
        !self.pending.load(Ordering::Acquire)
    }

    fn push(&self, item: T) {
        let mut deque = self.deque.lock();
        deque.push_back(item);
        self.pending.store(true, Ordering::Relaxed);
    }

    fn pop(&self) -> Option<T> {
        if self.is_empty() {
            return None;
        }

        let mut deque = self.deque.lock();
        deque.pop_back().map(|item| {
            self.pending.store(deque.len() > 0, Ordering::Relaxed);
            item
        })
    }

    fn steal(&self) -> Steal<T> {
        if self.is_empty() {
            return Steal::Empty;
        }

        let mut deque = match self.deque.try_lock() {
            Some(guard) => guard,
            None => return Steal::Retry,
        };

        let item = match deque.pop_front() {
            Some(item) => item,
            None => return Steal::Empty,
        };

        self.pending.store(deque.len() > 0, Ordering::Relaxed);
        Steal::Success(item)
    }

    fn steal_into(&self, dst_buffer: &Self) -> Steal<T> {
        if self.is_empty() {
            return Steal::Empty;
        }

        let mut deque = match self.deque.try_lock() {
            Some(guard) => guard,
            None => return Steal::Retry,
        };

        let item = match deque.pop_front() {
            Some(item) => item,
            None => return Steal::Empty,
        };

        let batch_size = (deque.len() / 2).min(16);
        if batch_size > 0 {
            let mut dst_deque = dst_buffer.deque.lock();
            dst_deque.extend(deque.drain(0..batch_size));
            dst_buffer.pending.store(true, Ordering::Relaxed);
        }

        self.pending.store(deque.len() > 0, Ordering::Relaxed);
        Steal::Success(item)
    }
}
