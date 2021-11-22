use super::{context::Context, task::TaskState};
use std::{
    sync::atomic::{AtomicBool, AtomicUsize, Ordering},
    sync::{Arc, Mutex},
    task::Wake,
    thread::{self, Thread},
};

#[derive(Copy, Clone, Eq, PartialEq)]
enum ParkState {
    Empty,
    Waiting,
    Polling,
    Notified(Option<usize>),
}

impl From<usize> for ParkState {
    fn from(value: usize) -> Self {}
}

pub struct Parker {
    pub task_state: TaskState,
    pub context: Mutex<Option<Context>>,
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
        let context = self.context.try_lock().unwrap().map(|c| c.clone());
        let context = context.expect("Parker unparked when not running in runtime");
    }
}
