use super::{
    executor::Executor,
    task::TaskState,
};
use std::{
    thread::{self, Thread},
    task::{Wake},
    sync::{Arc, Mutex},
    sync::atomic::{AtomicUsize, AtomicBool, Ordering},
};

pub struct Parker {
    pub task_state: TaskState,
    pub executor: Mutex<Option<Arc<Executor>>>,
    thread: Thread,
    notified: AtomicBool,
    state: AtomicUsize,
}

impl Default for Parker {
    fn default() -> Self {
        Self {
            task_state: TaskState::default(),
            executor: Mutex::new(None),
            thread: thread::current(),
            notified: AtomicBool::new(false),
            state: AtomicUsize::new(0),
        }
    }
}

impl Wake for Parker {
    fn wake(self: Arc<Self>) {
        if self.task_state.transition_to_scheduled() {
            self.unpark(None);
        }
    }

    fn wake_by_ref(self: &Arc<Self>) {
        if self.task_state.transition_to_scheduled() {
            Arc::clone(self).unpark(None);
        }
    }
}

impl Parker {
    pub fn unpark(&self, worker_index: Option<usize>) -> bool {

    }
}