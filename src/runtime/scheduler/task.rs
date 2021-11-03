use crate::internal::waker::AtomicWaker;
use super::{executor::Executor, thread::Thread};
use std::{
    any::Any,
    future::Future,
    mem::{drop, replace},
    panic::{catch_unwind, resume_unwind, AssertUnwindSafe},
    pin::Pin,
    sync::atomic::{fence, AtomicU8, Ordering},
    sync::Arc,
    task::{Context, Poll, Wake, Waker},
};
use try_lock::TryLock;

struct TaskState {
    state: AtomicU8,
}

impl TaskState {
    const IDLE: u8 = 0;
    const SCHEDULED: u8 = 1;
    const RUNNING: u8 = 2;
    const NOTIFIED: u8 = 3;

    fn new() -> Self {
        Self {
            state: AtomicU8::new(Self::IDLE),
        }
    }

    fn transition_to_scheduled(&self) -> bool {
        self.state
            .fetch_update(Ordering::Release, Ordering::Relaxed, |state| match state {
                Self::IDLE => Some(Self::SCHEDULED),
                Self::RUNNING => Some(Self::NOTIFIED),
                _ => None,
            })
            .map(|state| state == Self::IDLE)
            .unwrap_or(false)
    }

    fn transition_to_running(&self) {
        assert_eq!(self.state.load(Ordering::Relaxed), Self::SCHEDULED);
        self.state.store(Self::RUNNING, Ordering::Relaxed);
    }

    fn transition_to_idle(&self) -> bool {
        match self.state.compare_exchange(
            Self::RUNNING,
            Self::IDLE,
            Ordering::Release,
            Ordering::Relaxed,
        ) {
            Ok(_) => true,
            Err(state) => {
                assert_eq!(state, Self::NOTIFIED);
                fence(Ordering::Acquire);
                self.state.store(Self::RUNNING, Ordering::Relaxed);
                false
            }
        }
    }
}

enum TaskData<F: Future> {
    Pending(Pin<Box<F>>),
    Polling,
    Ready(Result<F::Output, Box<dyn Any + Send + 'static>>),
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
    pub fn spawn(
        future: F,
        executor: &Arc<Executor>,
        thread: Option<&Thread>,
    ) -> Arc<Self> {
        let task = Arc::new(Self {
            state: TaskState::new(),
            waker: AtomicWaker::new(),
            data: TryLock::new(TaskData::Pending(Box::pin(future))),
            executor: executor.clone(),
        });

        executor.task_begin();
        executor.schedule(task.clone(), thread, false);
        
        task
    }

    fn schedule(self: Arc<Self>) {
        let mut task = Some(self);
        let _ = Thread::try_with(|thread| {
            let task = task.take().unwrap();
            thread.executor.schedule(task, Some(thread), false);
        });

        if let Some(task) = task.take() {
            let executor = task.executor.clone();
            executor.schedule(task, None, false);
        }
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
    fn run(self: Arc<Self>, thread: &Thread);
}

impl<F> TaskRunnable for Task<F>
where
    F: Future + Send + 'static,
    F::Output: Send + 'static,
{
    fn run(self: Arc<Self>, thread: &Thread) {
        self.state.transition_to_running();

        let mut data = self.data.try_lock().unwrap();
        let mut future = match replace(&mut *data, TaskData::Polling) {
            TaskData::Pending(future) => future,
            _ => unreachable!(),
        };

        let poll_result = catch_unwind(AssertUnwindSafe(|| {
            let waker = Waker::from(self.clone());
            let mut ctx = Context::from_waker(&waker);
            future.as_mut().poll(&mut ctx)
        }));

        let result = match poll_result {
            Err(error) => Err(error),
            Ok(Poll::Ready(result)) => Ok(result),
            Ok(Poll::Pending) => {
                *data = TaskData::Pending(future);
                drop(data);

                if !self.state.transition_to_idle() {
                    thread.executor.schedule(self, Some(thread), true);
                }

                return;
            }
        };

        *data = TaskData::Ready(result);
        drop(data);

        self.waker.wake();
        thread.executor.task_complete();
    }
}

pub trait TaskJoinable<T> {
    fn poll_join(&self, ctx: &mut Context<'_>) -> Poll<T>;
    fn join(&self) -> T;
}

impl<F: Future> TaskJoinable<F::Output> for Task<F> {
    fn poll_join(&self, ctx: &mut Context<'_>) -> Poll<F::Output> {
        match self.waker.poll(Some(ctx)) {
            Poll::Ready(_) => Poll::Ready(self.join()),
            Poll::Pending => Poll::Pending,
        }
    }

    fn join(&self) -> F::Output {
        match replace(&mut *self.data.try_lock().unwrap(), TaskData::Joined) {
            TaskData::Ready(Ok(result)) => result,
            TaskData::Ready(Err(error)) => resume_unwind(error),
            _ => unreachable!(),
        }
    }
}
