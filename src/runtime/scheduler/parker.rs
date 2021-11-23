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
    fn from(value: usize) -> Self {
        match value & 0b11 {
            0 => Self::Empty,
            1 => Self::Waiting,
            2 => Self::Polling,
            3 => Self::Notified((value >> 2).checked_sub(1)),
            _ => unreachable!(),
        }
    }
}

impl Into<usize> for ParkState {
    fn into(self) -> usize {
        match self {
            Self::Empty => 0,
            Self::Waiting => 1,
            Self::Polling => 2,
            Self::Notified(index) => {
                let index = index.map(|i| i + 1).unwrap_or(0);
                3 | (index << 2)
            }
        }
    }
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
        self.state
            .fetch_update(Ordering::Release, Ordering::Relaxed, |state| {
                match ParkState::from(state) {
                    ParkState::Notified(_) => None,
                    _ => Some(ParkState::Notified(worker_index).into()),
                }
            })
            .ok()
            .and_then(|state| match ParkState::from(state) {
                ParkState::Empty => None,
                ParkState::Waiting => Some(true),
                ParkState::Polling => Some(false),
                _ => unreachable!(),
            })
            .map(|waiting| {
                let context = self.context.try_lock().unwrap().map(|c| c.clone());
                let context = context.expect("Parker unparked when not running in runtime");
                
            })
            .is_some()
    }
}
