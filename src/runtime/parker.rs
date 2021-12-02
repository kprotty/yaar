use super::task::TaskState;
use std::{
    sync::atomic::{AtomicUsize, AtomicBool, Ordering},
    sync::Arc,
    task::{Wake, Poll},
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
    pub queued: AtomicBool,
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
    pub fn new() -> Self {
        Self {
            state: AtomicUsize::new(0),
            thread: thread::current(),
            queued: AtomicBool::new(false),
            task_state: TaskState::default(),
        }
    }

    fn reset(&self) {
        let new_state = ParkState::Empty.into();
        self.state.store(new_state, Ordering::Relaxed);
    }

    pub fn poll(&self) -> Poll<Option<usize>> {
        match ParkState::from(self.state.load(Ordering::Acquire)) {
            ParkState::Notified(worker_index) => {
                self.reset();
                Poll::Ready(worker_index)
            }
            ParkState::Empty => Poll::Pending,
            ParkState::Waiting => unreachable!("Parker polling when waiting")
        }
    }

    pub fn park(&self) -> Option<usize> {
        if let Err(state) = self.state.compare_exchange(
            ParkState::Empty.into(),
            ParkState::Waiting.into(),
            Ordering::Acquire,
            Ordering::Acquire,
        ) {
            match ParkState::from(state) {
                ParkState::Notified(worker_index) => return worker_index,
                _ => unreachable!("invalid Parker state when waiting"),
            }
        }

        let worker_index = loop {
            thread::park();
            match ParkState::from(self.state.load(Ordering::Acquire)) {
                ParkState::Waiting => continue,
                ParkState::Notified(worker_index) => break worker_index,
                ParkState::Empty => unreachable!("invalid Parker state while waiting")
            }
        };

        self.reset();
        worker_index
    }

    pub fn unpark(&self, worker_index: Option<usize>) -> bool {
        self.state
            .fetch_update(
                Ordering::AcqRel,
                Ordering::Relaxed,
                |state| match ParkState::from(state) {
                    ParkState::Notified(_) => None,
                    _ => Some(ParkState::Notified(worker_index).into()),
                },
            )
            .map(|state| match ParkState::from(state) {
                ParkState::Empty => {},
                ParkState::Waiting => self.thread.unpark(),
                ParkState::Notified(_) => unreachable!(),
            })
            .is_ok()
    }
}
