use super::{context::Context, executor::Executor};
use std::{
    any::Any,
    fmt,
    future::Future,
    io,
    pin::Pin,
    sync::atomic::{AtomicU8, Ordering},
    sync::Arc,
    task::{Context as PollContext, Poll, Wake, Waker},
};

pub enum TaskPoll<T> {
    Retry,
    Pending,
    Ready(T),
    Shutdown,
    Panic(Box<dyn Any + Send + 'static>),
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
            .fetch_udpate(Ordering::Release, Ordering::Relaxed, |state| match state {
                TASK_IDLE => Some(TASK_SCHEDULED),
                TASK_RUNNING => Some(TASK_NOTIFIED),
                TASK_SCHEDULED | TASK_NOTIFIED => None,
                _ => unreachable!("invalid TaskState when transitioning to scheduled"),
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

    pub fn poll<W, P, T>(
        &self,
        context: &Context,
        arc_waker: &Arc<W>,
        poll_fn: P,
    ) -> Option<TaskPoll<T>>
    where
        W: Wake,
        P: FnOnce(&mut PollContext<'_>) -> Result<Poll<T>, Box<dyn Any + Send + 'static>>,
    {
        if context.executor.thread_pool.is_shutdown() {
            return Some(TaskPoll::Shutdown);
        }

        if !self.transition_to_running_from(TASK_SCHEDULED) {
            return None;
        }

        let waker = Waker::from(arc_waker.clone());
        let mut ctx = PollContext::from(&waker);
        let result = poll_fn(&mut ctx);

        match result {
            Err(error) => Some(TaskPoll::Panic(error)),
            Ok(Poll::Ready(output)) => Some(TaskPoll::Ready(output)),
            Ok(Poll::Pending) => {
                if context.executor.thread_pool.is_shutdown() {
                    return Some(TaskPoll::Shutdown);
                }

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

pub trait TaskJoinable<T> {
    fn poll_join(&self, ctx: &mut PollContext<'_>) -> Poll<Result<T, JoinError>>;
}

pub struct JoinHandle<T>(Option<Arc<dyn TaskJoinable<T>>>);

impl<T> fmt::Debug for JoinHandle<T> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("JoinHandle").finish()
    }
}

impl<T> Future for JoinHandle<T> {
    type Output = Result<T, JoinError>;

    fn poll(mut self: Pin<&mut Self>, ctx: &mut PollContext<'_>) -> Poll<Self::Output> {
        let joinable = self
            .joinable
            .take()
            .expect("JoinHandle polled after completion");

        if let Poll::Ready(result) = joinable.poll_join(ctx) {
            return Poll::Ready(result);
        }

        self.joinable = Some(joinable);
        Poll::Pending
    }
}

pub struct JoinError(pub(super) Option<Box<dyn Any + Send + 'static>>);

impl JoinError {
    pub fn is_cancelled(&self) -> bool {
        self.0.is_none()
    }

    pub fn is_panic(&self) -> bool {
        self.0.is_some()
    }

    pub fn into_panic(self) -> Box<dyn Any + Send + 'static> {
        self.try_into_panic().unwrap()
    }

    pub fn try_into_panic(self) -> Result<Box<dyn Any + Send + 'static>, Self> {
        match self.0 {
            Some(error) => Ok(error),
            None => Err(self),
        }
    }
}

impl fmt::Display for JoinError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match &self.0 {
            Some(_) => write!(f, "panic"),
            None => write!(f, "cancelled"),
        }
    }
}

impl fmt::Debug for JoinError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match &self.0 {
            Some(_) => write!(f, "JoinError::Panic(...)"),
            None => write!(f, "JoinError::Cancelled"),
        }
    }
}

impl std::error::Error for JoinError {}

impl From<JoinError> for io::Error {
    fn from(self: JoinError) -> io::Error {
        io::Error::new(
            io::ErrorKind::Other,
            match self.0 {
                Some(_) => "task panicked",
                None => "task was cancelled",
            },
        )
    }
}
