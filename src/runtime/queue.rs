use super::task::TaskRunnable;
use parking_lot::Mutex;
use std::{
    collections::VecDeque,
    sync::atomic::{AtomicBool, Ordering},
    sync::Arc,
};

#[derive(Copy, Clone, Eq, PartialEq)]
pub enum PopError {
    Empty,
    Contended,
}

pub type Task = Arc<dyn TaskRunnable>;

pub struct Queue {
    pending: AtomicBool,
    deque: Mutex<VecDeque<Task>>,
}

impl Default for Queue {
    fn default() -> Self {
        Self {
            pending: AtomicBool::new(false),
            deque: Mutex::new(VecDeque::new()),
        }
    }
}

impl Queue {
    pub fn inject(&self, tasks: impl Iterator<Item = Task>) {
        self.push(tasks)
    }

    pub fn push(&self, tasks: impl Iterator<Item = Task>) {
        let mut deque = self.deque.lock();
        deque.extend(tasks);

        if !self.pending.load(Ordering::Relaxed) && deque.len() > 0 {
            self.pending.store(true, Ordering::Relaxed);
        }
    }

    pub fn pending(&self) -> bool {
        self.pending.load(Ordering::Acquire)
    }

    pub fn pop(&self) -> Option<Task> {
        if !self.pending.load(Ordering::Relaxed) {
            return None;
        }

        let mut deque = self.deque.lock();
        let task = deque.pop_front()?;

        if deque.len() == 0 {
            self.pending.store(false, Ordering::Relaxed);
        }

        Some(task)
    }

    pub fn steal(&self, target: &Self) -> Result<Task, PopError> {
        if !target.pending.load(Ordering::Relaxed) {
            return Err(PopError::Empty);
        }

        let mut target_deque = match target.deque.try_lock() {
            Some(guard) => guard,
            None => return Err(PopError::Contended),
        };

        if !target.pending.load(Ordering::Relaxed) {
            return Err(PopError::Empty);
        }

        let size = target_deque.len();
        assert_ne!(size, 0);

        let grab = (size - (size / 2)).min(64);
        if grab == size {
            target.pending.store(false, Ordering::Relaxed);
        }

        let mut target_iter = target_deque.drain(0..grab);
        let task = target_iter.next().unwrap();

        let mut deque = self.deque.lock();
        deque.extend(target_iter);
        self.pending.store(deque.len() > 0, Ordering::Relaxed);

        Ok(task)
    }
}
