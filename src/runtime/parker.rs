use super::task::TaskState;
use std::{
    sync::atomic::{AtomicUsize, Ordering},
    sync::Arc,
    task::Wake,
    thread::{self, Thread},
};

#[derive(Copy, Clone, Eq, PartialEq)]
enum ParkState {
    Empty,
    Waiting,
    Notified(Option<usize>),
}

impl Into<usize> for ParkState {
    fn into(self) -> usize {
        match self {
            Self::Empty => 0,
            Self::Waiting => 1,
            Self::Notified(worker_index) => {
                let worker_index = worker_index.map(|i| i + 1).unwrap_or(0);
                2 | (worker_index << 2)
            }
        }
    }
}

impl From<usize> for ParkState {
    fn from(value: usize) -> Self {
        match value & 0b11 {
            0 => Self::Empty,
            1 => Self::Waiting,
            2 => Self::Notified((value >> 2).checked_sub(1)),
            _ => unreachable!("invalid ParkState"),
        }
    }
}

pub struct Parker {
    state: AtomicUsize,
    thread: Thread,
    pub task_state: TaskState,
}

impl Wake for Parker {
    fn wake(self: Arc<Self>) {
        self.wake_by_ref();
    }

    fn wake_by_ref(self: &Arc<Self>) {
        if self.task_state.transition_to_scheduled() {
            self.unpark(None);
        }
    }
}

impl Parker {
    pub fn poll(&self) -> Option<Option<usize>> {
        match ParkState::from(self.state.load(Ordering::Acquire)) {
            ParkState::Notified(worker_index) => Some(worker_index),
            _ => None,
        }
    }

    pub fn park(&self) -> Option<usize> {
        let worker_index = self
            .state
            .fetch_update(
                Ordering::Acquire,
                Ordering::Acquire,
                |state| match ParkState::from(state) {
                    ParkState::Waiting => unreachable!("parking when already parked"),
                    ParkState::Empty => Some(ParkState::Waiting.into()),
                    ParkState::Notified(_) => None,
                },
            )
            .map(|_| loop {
                thread::park();
                match self.poll() {
                    Some(worker_index) => break worker_index,
                    None => continue,
                }
            })
            .unwrap_or(|state| match ParkState::from(state) {
                ParkState::Notified(worker_index) => worker_index,
                _ => unreachable!("invalid ParkState when notified"),
            });

        let new_state = ParkState::Empty.into();
        self.state.store(new_state, Ordering::Relaxed);
        worker_index
    }

    fn unpark(&self, worker_index: Option<usize>) {
        self.state
            .fetch_update(
                Ordering::Release,
                Ordering::Relaxed,
                |state| match ParkState::from(state) {
                    ParkState::Notified(_) => None,
                    _ => Some(ParkState::Notified(worker_index).into()),
                },
            )
            .map(|state| match ParkState::from(state) {
                ParkState::Waiting => self.thread.unpark(),
                _ => {}
            })
            .unwrap_or(())
    }
}
