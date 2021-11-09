use super::{context::Context, executor::Executor, poller::Poller, task::TaskState};
use parking_lot::{Condvar, Mutex};
use std::{
    mem::drop,
    sync::atomic::{AtomicUsize, Ordering},
    sync::Arc,
    time::Duration,
};

#[derive(Copy, Clone, Eq, PartialEq, Debug)]
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
            Self::Notified(worker_index) => {
                let index = worker_index.map(|i| i + 1).unwrap_or(0);
                assert!(index < (usize::MAX >> 2));
                3 | (index << 2)
            }
        }
    }
}

pub struct Parker {
    executor: Arc<Executor>,
    pub task_state: TaskState,
    state: AtomicUsize,
    mutex: Mutex<()>,
    condvar: Condvar,
}

impl Parker {
    pub fn new(executor: Arc<Executor>) -> Self {
        Self {
            executor,
            task_state: TaskState::new(),
            state: AtomicUsize::new(0),
            mutex: Mutex::new(()),
            condvar: Condvar::new(),
        }
    }

    pub fn poll_unparked(&self) -> Option<Option<usize>> {
        let worker_index = match ParkState::from(self.state.load(Ordering::Acquire)) {
            ParkState::Notified(worker_index) => worker_index,
            _ => return None,
        };

        self.state.store(ParkState::Empty.into(), Ordering::Relaxed);
        Some(worker_index)
    }

    #[cold]
    pub fn park(
        &self,
        poller: &mut Poller,
        context: &Context,
        timeout: Option<Duration>,
    ) -> Option<usize> {
        let poll_network_guard = poller.try_poll_network(context, &self.executor.net_poller);

        let is_polling = poll_network_guard.is_some();
        let wait_state = [ParkState::Waiting, ParkState::Polling][is_polling as usize];

        if let Err(_) = self.state.compare_exchange(
            ParkState::Empty.into(),
            wait_state.into(),
            Ordering::Acquire,
            Ordering::Relaxed,
        ) {
            return self.poll_unparked().unwrap();
        }

        match poll_network_guard {
            Some(park_polling) => park_polling.poll(timeout),
            None => self.park_waiting(timeout),
        }

        if let Err(_) = self.state.compare_exchange(
            wait_state.into(),
            ParkState::Empty.into(),
            Ordering::Release,
            Ordering::Relaxed,
        ) {
            return self.poll_unparked().unwrap();
        }

        None
    }

    pub fn unpark(&self, worker_index: Option<usize>) -> bool {
        self.state
            .fetch_update(
                Ordering::Release,
                Ordering::Relaxed,
                |state| match ParkState::from(state) {
                    ParkState::Notified(_) => None,
                    ParkState::Empty if worker_index.is_some() => None,
                    _ => Some(ParkState::Notified(worker_index).into()),
                },
            )
            .map(|state| {
                match ParkState::from(state) {
                    ParkState::Polling => self.executor.net_poller.notify(),
                    ParkState::Waiting => self.unpark_waiting(),
                    _ => return false,
                }
                true
            })
            .ok()
            .unwrap_or(false)
    }

    #[cold]
    fn park_waiting(&self, timeout: Option<Duration>) {
        let timeout = timeout.unwrap_or_else(|| {
            let config = &self.executor.thread_pool.config;
            config.keep_alive.unwrap()
        });

        let mut mutex = self.mutex.lock();
        match ParkState::from(self.state.load(Ordering::Relaxed)) {
            ParkState::Waiting => {}
            ParkState::Notified(_) => return,
            _ => unreachable!("Parker waiting on condvar with invalid state"),
        }

        let _ = self.condvar.wait_for(&mut mutex, timeout);
        match ParkState::from(self.state.load(Ordering::Relaxed)) {
            ParkState::Waiting => {}
            ParkState::Notified(_) => return,
            _ => unreachable!("Parker waiting on condvar with invalid state"),
        }
    }

    #[cold]
    fn unpark_waiting(&self) {
        drop(self.mutex.lock());
        self.condvar.notify_one();
    }
}
