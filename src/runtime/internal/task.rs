use super::{context::Context, executor::Executor};
use crate::sync::internal::waker::AtomicWaker;
use std::{
    any::Any,
    future::Future,
    mem::{drop, replace},
    panic::{catch_unwind, resume_unwind, AssertUnwindSafe},
    pin::Pin,
    sync::atomic::{AtomicU8, Ordering},
    sync::Arc,
    task::{Context as PollContext, Poll, Wake, Waker},
};
use try_lock::TryLock;

const TASK_IDLE: u8 = 0;
const TASK_SCHEDULED: u8 = 1;
const TASK_RUNNING: u8 = 2;
const TASK_NOTIFIED: u8 = 3;

pub struct TaskState {
    state: AtomicU8,
}

impl TaskState {
    pub fn new() -> Self {
        Self {
            state: AtomicU8::new(TASK_IDLE),
        }
    }

    pub fn transition_to_scheduled(&self) -> bool {
        self.state
            .fetch_update(Ordering::Release, Ordering::Relaxed, |state| match state {
                TASK_IDLE => Some(TASK_SCHEDULED),
                TASK_RUNNING => Some(TASK_NOTIFIED),
                _ => None,
            })
            .map(|state| state == TASK_IDLE)
            .unwrap_or(false)
    }

    pub fn transition_to_running(&self) -> bool {
        match self.state.load(Ordering::Acquire) {
            TASK_SCHEDULED => {}
            TASK_IDLE => return false,
            _ => unreachable!("Task transitioned to running with invalid state"),
        }

        self.state.store(TASK_RUNNING, Ordering::Relaxed);
        true
    }

    pub fn transition_to_idle(&self) -> bool {
        match self.state.compare_exchange(
            TASK_RUNNING,
            TASK_IDLE,
            Ordering::Release,
            Ordering::Relaxed,
        ) {
            Ok(_) => true,
            Err(TASK_NOTIFIED) => false,
            Err(_) => unreachable!("Task transitioned to idle with invalid state"),
        }
    }

    pub fn transition_to_scheduled_from_notified(&self) {
        assert_eq!(self.state.load(Ordering::Acquire), TASK_NOTIFIED);
        self.state.store(TASK_SCHEDULED, Ordering::Relaxed);
    }
}

pub type TaskError = Box<dyn Any + Send + 'static>;

enum TaskData<F: Future> {
    Pending(Pin<Box<F>>),
    Polling,
    Ready(Result<F::Output, TaskError>),
    Joined,
}

pub struct Task<F: Future> {
    state: TaskState,
    waker: AtomicWaker,
    data: TryLock<TaskData<F>>,
    executor: Arc<Executor>,
}

impl<F> Task<F>
where
    F: Future + Send + 'static,
    F::Output: Send + 'static,
{
    pub fn spawn(future: F, executor: &Arc<Executor>, context: Option<&Context>) -> Arc<Self> {
        let task = Arc::new(Self {
            state: TaskState::new(),
            waker: AtomicWaker::default(),
            data: TryLock::new(TaskData::Pending(Box::pin(future))),
            executor: executor.clone(),
        });

        executor.thread_pool.task_begin();
        assert!(task.state.transition_to_scheduled());
        executor.schedule(task.clone(), context, false);

        task
    }

    fn schedule(self: Arc<Self>) {
        if let Some(context_ref) = Context::try_current() {
            let context = context_ref.as_ref();
            context.executor.schedule(self, Some(context), false);
            return;
        }

        let executor = self.executor.clone();
        executor.schedule(self, None, false);
    }
}

impl<F> Wake for Task<F>
where
    F: Future + Send + 'static,
    F::Output: Send + 'static,
{
    fn wake(self: Arc<Self>) {
        if self.state.transition_to_scheduled() {
            self.schedule();
        }
    }

    fn wake_by_ref(self: &Arc<Self>) {
        if self.state.transition_to_scheduled() {
            self.clone().schedule();
        }
    }
}

pub trait TaskRunnable: Send + Sync {
    fn run(self: Arc<Self>, context: &Context);
}

impl<F> TaskRunnable for Task<F>
where
    F: Future + Send + 'static,
    F::Output: Send + 'static,
{
    fn run(self: Arc<Self>, context: &Context) {
        let is_running = self.state.transition_to_running();
        assert!(is_running);

        let mut data = self.data.try_lock().unwrap();
        let mut future = match replace(&mut *data, TaskData::Polling) {
            TaskData::Pending(future) => future,
            _ => unreachable!(),
        };

        let poll_result = catch_unwind(AssertUnwindSafe(|| {
            let waker = Waker::from(self.clone());
            let mut ctx = PollContext::from_waker(&waker);
            future.as_mut().poll(&mut ctx)
        }));

        let result = match poll_result {
            Err(error) => Err(error),
            Ok(Poll::Ready(result)) => Ok(result),
            Ok(Poll::Pending) => {
                *data = TaskData::Pending(future);
                drop(data);

                if !self.state.transition_to_idle() {
                    self.state.transition_to_scheduled_from_notified();
                    context.executor.schedule(self, Some(context), true);
                }

                return;
            }
        };

        *data = TaskData::Ready(result);
        drop(data);

        self.waker.wake();
        context.executor.thread_pool.task_complete();
    }
}

pub trait TaskJoinable<T> {
    fn poll_join(&self, ctx: &mut PollContext<'_>) -> Poll<T>;
}

impl<F: Future> TaskJoinable<F::Output> for Task<F> {
    fn poll_join(&self, ctx: &mut PollContext<'_>) -> Poll<F::Output> {
        match self.waker.poll(Some(ctx)) {
            Poll::Ready(_) => {}
            Poll::Pending => return Poll::Pending,
        }

        let result = match replace(&mut *self.data.try_lock().unwrap(), TaskData::Joined) {
            TaskData::Ready(Err(error)) => resume_unwind(error),
            TaskData::Ready(Ok(result)) => result,
            _ => unreachable!(),
        };

        Poll::Ready(result)
    }
}
