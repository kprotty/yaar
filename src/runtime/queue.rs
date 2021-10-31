use super::task::TaskRunnable;
use crossbeam_deque::{Steal, Stealer, Worker};
use parking_lot::Mutex;
use std::{
    cell::RefCell,
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

#[derive(Default)]
pub struct Injector {
    pending: AtomicBool,
    deque: Mutex<VecDeque<Task>>,
}

impl Injector {
    pub fn pending(&self) -> bool {
        self.pending.load(Ordering::Acquire)
    }

    pub fn inject(&self, tasks: impl Iterator<Item = Task>) {
        let mut deque = self.deque.lock();
        deque.extend(tasks);

        if !self.pending.load(Ordering::Relaxed) && deque.len() > 0 {
            self.pending.store(true, Ordering::Relaxed);
        }
    }
}

pub struct Producer {
    worker: Worker<Task>,
    injected: RefCell<VecDeque<Task>>,
}

impl Producer {
    pub fn push(&self, tasks: impl Iterator<Item = Task>) {
        tasks.for_each(|task| self.worker.push(task));
    }

    pub fn pop(&self, queue: &Queue, be_fair: bool) -> Option<Task> {
        match be_fair {
            true => match queue.stealer.steal() {
                Steal::Success(task) => Some(task),
                _ => None,
            },
            _ => self.worker.pop(),
        }
    }

    pub fn steal(&self, queue: &Queue) -> Result<Task, PopError> {
        match queue.stealer.steal_batch_and_pop(&self.worker) {
            Steal::Success(task) => Ok(task),
            Steal::Empty => Err(PopError::Empty),
            Steal::Retry => Err(PopError::Contended),
        }
    }

    pub fn consume(&self, injector: &Injector) -> Option<Task> {
        if !injector.pending.load(Ordering::Relaxed) {
            return None;
        }

        let mut injected = self.injected.borrow_mut();
        {
            let mut deque = injector.deque.lock();
            if !injector.pending.load(Ordering::Relaxed) {
                return None;
            }

            mem::swap(&mut *deque, &mut *injected);
            injector.pending.store(false, Ordering::Relaxed);
        }

        injected.pop_front().map(|task| {
            injected.drain(..).for_each(|task| self.worker.push(task));
            task
        })
    }
}

pub struct Queue {
    stealer: Stealer<Task>,
    producer: TryLock<Option<Producer>>,
}

impl Default for Queue {
    fn default() -> Self {
        let worker = Worker::new_lifo();
        let stealer = worker.stealer();

        Self {
            stealer,
            producer: TryLock::new(Some(Producer {
                worker,
                injected: RefCell::new(VecDeque::new()),
            })),
        }
    }
}

impl Queue {
    pub fn pending(&self) -> bool {
        self.stealer.len() > 0
    }

    pub fn swap_producer(&self, new_producer: Option<Producer>) -> Option<Producer> {
        let mut producer = self
            .producer
            .try_lock()
            .expect("Multiple threads trying to update a Queue's producer");

        mem::replace(&mut *producer, new_producer)
    }
}
