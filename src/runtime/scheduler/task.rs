use std::{
    sync::atomic::{AtomicU8, Ordering},
    sync::Arc,
    any::Any,
    future::Future,
    pin::Pin,
    task::{Poll, Context as PollContext, Wake, Waker},
};

pub type TaskError = Box<dyn Any + Send + 'static>;

pub enum TaskPoll<T> {
    Retry,
    Pending,
    Ready(T),
    Error(TaskError),
}

const TASK_IDLE: u8 = 0;
const TASK_SCHEDULED: u8 = 1;
const TASK_RUNNING: u8 = 2;
const TASK_NOTIFIED: u8 = 3;

#[derive(Default)]
pub struct TaskState {
    state: AtomicU8,
}

impl TaskState {
    pub fn transition_to_scheduled(&self) -> bool {
        self.state
            .fetch_udpate(Ordering::Release, Ordering::Relaxed, |state| {
                match state {
                    TASK_IDLE => Some(TASK_SCHEDULED),
                    TASK_RUNNING => Some(TASK_NOTIFIED),
                    TASK_SCHEDULED | TASK_NOTIFIED => None,
                    _ => unreachable!("invalid TaskState when transitioning to scheduled"),
                }
            })
            .ok()
            .map(|state| state == TASK_IDLE)
            .unwrap_or(false)
    }

    fn transition_to_running_from(&self, runnable_state: u8) -> bool {
        let state = self.state.load(Ordering::Acquire);
        if state != runnable_state {
            return false;
        }

        self.state.store(TASK_RUNNING, Ordering::Relaxed);
        true
    }

    fn transition_to_idle(&self) -> bool {
        match self.state.compare_exchange(
            TASK_RUNNING,
            TASK_IDLE,
            Ordering::Release,
            Ordering::Relaxed,
        ) {
            Ok(_) => true,
            Err(TASK_NOTIFIED) => false,
            Err(_) => unreachable!("invalid TaskState when transitioning to idle"),
        }
    }

    pub fn poll<F: Future>(&self, waker: impl Wake, future: Pin<&mut F>) -> Option<TaskPoll<F::Output>> {
        if !self.transition_to_running_from(TASK_SCHEDULED) {
            return None;
        }

        let waker = Waker::from(waker);
        let mut ctx = PollContext::from(&waker);
        let result = catch_unwind(AssertUnwindSafe(|| future.poll(&mut ctx)));
        
        match result {
            Err(error) => Some(TaskPoll::Error(error)),
            Ok(Poll::Ready(output)) => Some(TaskPoll::Ready(output)),
            Ok(Poll::Pending) => {
                if self.transition_to_idle() {
                    return Some(TaskPoll::Pending);
                }

                assert!(self.transition_to_running_from(TASK_NOTIFIED));
                Some(TaskPoll::Retry)
            }
        }
    }
}

pub trait TaskRunnable: Send + Sync {
    fn run(self: Arc<Self>, context: &Context);
}
