use super::{context::Context, executor::Executor, queue::Runnable, task::TaskState};
use crate::io::driver::{PollEvents, PollGuard};
use parking_lot::{Condvar, Mutex};
use std::{
    collections::VecDeque,
    mem::{drop, replace},
    sync::atomic::{AtomicUsize, Ordering},
    sync::Arc,
    time::Duration,
};
use try_lock::TryLock;

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

struct PollState {
    poll_events: PollEvents,
    poll_ready: VecDeque<Runnable>,
}

pub struct Parker {
    executor: Arc<Executor>,
    pub task_state: TaskState,
    state: AtomicUsize,
    mutex: Mutex<()>,
    condvar: Condvar,
    poll_state: TryLock<PollState>,
}

impl Parker {
    pub fn new(executor: Arc<Executor>) -> Self {
        Self {
            executor,
            task_state: TaskState::new(),
            state: AtomicUsize::new(0),
            mutex: Mutex::new(()),
            condvar: Condvar::new(),
            poll_state: TryLock::new(PollState {
                poll_events: PollEvents::new(),
                poll_ready: VecDeque::new(),
            }),
        }
    }

    pub fn poll(&self) -> Option<Option<usize>> {
        let worker_index = match ParkState::from(self.state.load(Ordering::Acquire)) {
            ParkState::Notified(worker_index) => worker_index,
            _ => return None,
        };

        self.state.store(ParkState::Empty.into(), Ordering::Relaxed);
        Some(worker_index)
    }

    #[cold]
    pub fn park(&self, timeout: Option<Duration>) -> Option<usize> {
        let poll_guard = self.executor.io_driver.try_poll();
        let wait_state = [ParkState::Waiting, ParkState::Polling][poll_guard.is_some() as usize];

        if let Err(_) = self.state.compare_exchange(
            ParkState::Empty.into(),
            wait_state.into(),
            Ordering::Acquire,
            Ordering::Relaxed,
        ) {
            return self.poll().unwrap();
        }

        match poll_guard {
            None => self.park_waiting(timeout),
            Some(guard) => self.park_polling(guard, timeout, |ready| {
                self.executor.inject(ready.drain(..));
            }),
        }

        if let Err(_) = self.state.compare_exchange(
            wait_state.into(),
            ParkState::Empty.into(),
            Ordering::Release,
            Ordering::Relaxed,
        ) {
            return self.poll().unwrap();
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
            .map(|state| match ParkState::from(state) {
                ParkState::Polling => {
                    self.unpark_polling();
                    true
                }
                ParkState::Waiting => {
                    self.unpark_waiting();
                    true
                }
                _ => false,
            })
            .ok()
            .unwrap_or(false)
    }

    #[cold]
    pub fn park_polling(
        &self,
        mut poll_guard: PollGuard<'_>,
        timeout: Option<Duration>,
        f: impl FnOnce(&mut VecDeque<Runnable>),
    ) {
        let context_ref = Context::current();
        let context = context_ref.as_ref();

        let mut poll_state = self.poll_state.try_lock().unwrap();

        let poll_ready = replace(&mut poll_state.poll_ready, VecDeque::new());
        let intercept = context.intercept.borrow_mut().replace(poll_ready);
        assert!(intercept.is_none());

        poll_guard.poll(&mut poll_state.poll_events, timeout);
        drop(poll_guard);

        let intercept = context.intercept.borrow_mut().take();
        poll_state.poll_ready = intercept.unwrap();

        poll_state.poll_events.process(&self.executor.io_driver);
        f(&mut poll_state.poll_ready);
    }

    #[cold]
    fn unpark_polling(&self) {
        self.executor.io_driver.notify();
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

        let timed_out = self.condvar.wait_for(&mut mutex, timeout).timed_out();
        match ParkState::from(self.state.load(Ordering::Relaxed)) {
            ParkState::Waiting => {}
            ParkState::Notified(_) => return,
            _ => unreachable!("Parker waiting on condvar with invalid state"),
        }

        if timed_out {
            let _ = self.task_state.transition_to_scheduled();
        }
    }

    #[cold]
    fn unpark_waiting(&self) {
        drop(self.mutex.lock());
        self.condvar.notify_one();
    }
}
