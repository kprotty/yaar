use super::task::TaskRunnable;
use parking_lot::Mutex;
use std::{
    cell::RefCell,
    collections::VecDeque,
    mem,
    sync::atomic::{AtomicBool, Ordering},
    sync::Arc,
};
use try_lock::TryLock;

pub type Runnable = Arc<dyn TaskRunnable>;

#[derive(Default)]
pub struct Injector {
    pending: AtomicBool,
    deque: Mutex<VecDeque<Runnable>>,
}

impl Injector {
    pub fn pending(&self) -> bool {
        self.pending.load(Ordering::Relaxed)
    }

    pub fn push(&self, runnables: impl Iterator<Item = Runnable>) {
        let mut deque = self.deque.lock();
        deque.extend(runnables);
        self.pending.store(true, Ordering::Relaxed);
    }
}

pub struct Queue {
    stealer: crossbeam_deque::Stealer<Runnable>,
    producer: TryLock<Option<Producer>>,
}

impl Default for Queue {
    fn default() -> Self {
        let worker = crossbeam_deque::Worker::new_lifo();
        let stealer = worker.stealer();

        Self {
            stealer: stealer.clone(),
            producer: TryLock::new(Some(Producer {
                worker,
                stealer,
                injected: RefCell::new(VecDeque::new()),
            })),
        }
    }
}

impl Queue {
    pub fn pending(&self) -> bool {
        !self.stealer.is_empty()
    }

    pub fn swap_producer(&self, new_producer: Option<Producer>) -> Option<Producer> {
        let mut producer = self
            .producer
            .try_lock()
            .expect("Multiple threads accessing the same Queue's Producer");

        mem::replace(&mut *producer, new_producer)
    }
}

pub type Steal = crossbeam_deque::Steal<Runnable>;

pub struct Producer {
    worker: crossbeam_deque::Worker<Runnable>,
    stealer: crossbeam_deque::Stealer<Runnable>,
    injected: RefCell<VecDeque<Runnable>>,
}

impl Producer {
    pub fn push(&self, runnables: impl Iterator<Item = Runnable>) {
        runnables.for_each(|runnable| self.worker.push(runnable));
    }

    pub fn pop(&self, be_fair: bool) -> Option<Runnable> {
        if be_fair {
            self.stealer.steal().success()
        } else {
            self.worker.pop()
        }
    }

    pub fn steal(&self, queue: &Queue) -> Steal {
        match queue.stealer.steal_batch_and_pop(&self.worker) {
            crossbeam_deque::Steal::Success(runnable) => Steal::Success(runnable),
            crossbeam_deque::Steal::Empty => Steal::Empty,
            crossbeam_deque::Steal::Retry => Steal::Retry,
        }
    }

    pub fn consume(&self, injector: &Injector) -> Option<Runnable> {
        if !injector.pending() {
            return None;
        }

        let mut deque = injector.deque.lock();
        if !injector.pending() {
            return None;
        }

        let mut injected = self.injected.borrow_mut();
        assert_eq!(injected.len(), 0);
        mem::swap(&mut *injected, &mut *deque);

        assert_eq!(deque.len(), 0);
        injector.pending.store(false, Ordering::Relaxed);
        mem::drop(deque);

        let runnable = injected.pop_front().unwrap();
        self.push(injected.drain(..));
        Some(runnable)
    }
}
