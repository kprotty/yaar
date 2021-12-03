use std::{
    cell::Cell,
    collections::VecDeque,
    sync::atomic::{AtomicBool, Ordering},
    sync::{Arc, Mutex},
};

pub enum Steal<T> {
    Success(T),
    Empty,
    Retry,
}

#[derive(Copy, Clone)]
enum Order {
    Fifo,
    Lifo,
}

#[derive(Default)]
struct Buffer<T> {
    pending: AtomicBool,
    queue: Mutex<VecDeque<T>>,
}

impl<T> Buffer<T> {
    fn push(&self, item: T) {
        let mut queue = self.queue.lock().unwrap();
        queue.push_back(item);
        self.pending.store(true, Ordering::Relaxed);
    }

    fn pop(&self, order: Order) -> Option<T> {
        if !self.pending.load(Ordering::Relaxed) {
            return None;
        }

        let mut queue = self.queue.lock().unwrap();
        let item = match order {
            Order::Fifo => queue.pop_front(),
            Order::Lifo => queue.pop_back(),
        };

        self.pending.store(queue.len() > 0, Ordering::Relaxed);
        item
    }

    fn steal_into(&self, dst: &Self) -> Steal<T> {
        if !self.pending.load(Ordering::Acquire) {
            return Steal::Empty;
        }

        let mut queue = match self.queue.try_lock() {
            Ok(queue) => queue,
            Err(_) => return Steal::Retry,
        };

        let item = match queue.pop_back() {
            Some(item) => item,
            None => return Steal::Empty,
        };

        let take = (queue.len() / 2).min(16);
        if take > 0 {
            let mut dst_queue = dst.queue.lock().unwrap();
            dst_queue.extend(queue.drain(0..take));
            dst.pending.store(true, Ordering::Relaxed);
        }

        self.pending.store(queue.len() > 0, Ordering::Relaxed);
        Steal::Success(item)
    }
}

#[derive(Default)]
pub struct Injector<T> {
    buffer: Buffer<T>,
}

impl<T> Injector<T> {
    pub fn push(&self, item: T) {
        self.buffer.push(item);
    }

    pub fn steal_batch_and_pop(&self, worker: &Worker<T>) -> Steal<T> {
        self.buffer.steal_into(&worker.buffer)
    }

    pub fn is_empty(&self) -> bool {
        !self.buffer.pending.load(Ordering::Acquire)
    }
}

pub struct Worker<T> {
    buffer: Arc<Buffer<T>>,
    _non_send: Cell<()>,
}

impl<T> Worker<T> {
    pub fn new_lifo() -> Self {
        Self {
            buffer: Arc::new(Buffer::default()),
            _non_send: Cell::new(()),
        }
    }

    pub fn push(&self, item: T) {
        self.buffer.push(item);
    }

    pub fn pop(&self) -> Option<T> {
        self.buffer.pop(Order::Lifo)
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
        self.buffer.pop(Order::Fifo)
    }

    pub fn steal_batch_and_pop(&self, worker: &Worker<T>) -> Steal<T> {
        if ptr::eq(&self.buffer, &worker.buffer) {
            self.steal(worker)
        } else {
            self.buffer.steal_into(&worker.buffer)
        }
    }

    pub fn is_empty(&self) -> bool {
        !self.buffer.pending.load(Ordering::Acquire)
    }
}