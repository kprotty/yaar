use super::task::TaskRunnable;
use parking_lot::Mutex;
use std::{
    collections::VecDeque,
    mem, ptr,
    sync::atomic::{AtomicBool, Ordering},
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

        if !self.pending.load(Ordering::Relaxed) && deque.len() > 0 {
            self.pending.store(true, Ordering::Relaxed);
        }
    }
}

pub type Steal = crossbeam_deque::Steal<Runnable>;

pub struct Queue {
    worker: crossbeam_deque::Worker<Runnable>,
    stealer: crossbeam_deque::Stealer<Runnable>,
    injected: TryLock<VecDeque<Runnable>>,
}

impl Default for Queue {
    fn default() -> Self {
        let worker = crossbeam_deque::Worker::new_lifo();
        let stealer = worker.stealer();

        Self {
            worker,
            stealer,
            injected: TryLock::new(VecDeque::new()),
        }
    }
}

impl Queue {
    pub fn push(&self, runnables: impl Iterator<Item = Runnable>) {
        runnables.for_each(|runnable| self.worker.push(runnable));
    }

    pub fn pop(&self, be_fair: bool) -> Option<Runnable> {
        match be_fair {
            false => self.worker.pop(),
            _ => match self.stealer.steal() {
                crossbeam_deque::Steal::Success(runnable) => Some(runnable),
                _ => None,
            },
        }
    }

    pub fn steal(&self, queue: &Self) -> Steal {
        if ptr::eq(self, queue) {
            return Steal::Empty;
        }

        match queue.stealer.steal_batch_and_pop(&self.worker) {
            crossbeam_deque::Steal::Success(runnable) => Steal::Success(runnable),
            crossbeam_deque::Steal::Empty => Steal::Empty,
            crossbeam_deque::Steal::Retry => Steal::Retry,
        }
    }

    pub fn consume(&self, injector: &Injector) -> Steal {
        if !injector.pending() {
            return Steal::Empty;
        }

        let mut deque = injector.deque.lock();
        if !injector.pending() {
            return Steal::Empty;
        }

        let mut injected = self.injected.try_lock().unwrap();
        assert_eq!(injected.len(), 0);
        mem::swap(&mut *injected, &mut *deque);

        assert_eq!(deque.len(), 0);
        injector.pending.store(false, Ordering::Relaxed);
        mem::drop(deque);

        let runnable = injected.pop_first().unwrap();
        self.push(injected.drain(..));
        Steal::Success(runnable)
    }
}
