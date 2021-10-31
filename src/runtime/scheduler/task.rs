use super::{executor::Executor, thread::Thread};
use crate::sync::waker::AtomicWaker;
use parking_lot::Mutex;
use std::{
    any::Any,
    future::Future,
    iter::once,
    mem, panic,
    pin::Pin,
    sync::atomic::{AtomicU8, Ordering},
    sync::Arc,
    task::{Context, Poll, Wake, Waker},
};

#[derive(Copy, Clone, Eq, PartialEq, Debug)]
enum TaskStatus {
    Idle = 0,
    Scheduled = 1,
    Running = 2,
    Notified = 3,
}

impl Into<u8> for TaskStatus {
    fn into(self) -> u8 {
        self.status as u8
    }
}

impl From<u8> for TaskStatus {
    fn from(value: u8) -> Self {
        match value {
            0 => TaskStatus::Idle,
            1 => TaskStatus::Scheduled,
            2 => TaskStatus::Running,
            3 => TaskStatus::Notified,
            _ => unreachable!(),
        }
    }
}

struct TaskState {
    status: AtomicU8,
}

impl From<TaskStatus> for TaskState {
    fn from(status: TaskStatus) -> Self {
        Self {
            status: AtomicU8::new(status.into()),
        }
    }
}

impl TaskState {
    fn transition_to_schedule(&self) -> bool {
        self.status
            .fetch_update(Ordering::Release, Ordering::Relaxed, |status| {
                Some(match TaskStatus::from(status) {
                    TaskStatus::Idle => TaskStatus::Scheduled.into(),
                    TaskStatus::Running => TaskStatus::Notified.into(),
                    _ => return None,
                })
            })
            .map(|status| match TaskStatus::from(status) {
                TaskStatus::Idle => true,
                TaskStatus::Running => false,
                _ => unreachable!(),
            })
            .unwrap_or(false)
    }

    fn transition_to_running(&self) {
        let mut status: TaskStatus = self.status.load(Ordering::Acquire).into();
        assert_eq!(status, TaskStatus::Scheduled);

        status = TaskStatus::Running;
        self.status.store(status.into(), Ordering::Relaxed);
    }

    fn transition_to_idle(&self) -> bool {
        match self.status.compare_exchange(
            TaskStatus::Running.into(),
            TaskStatus::Idle.into(),
            Ordering::AcqRel,
            Ordering::Acquire,
        ) {
            Ok(_) => true,
            Err(e) => {
                let status: TaskStatus = e.into();
                assert_eq!(status, TaskStatus::Notified);
                false
            }
        }
    }
}

type TaskError = Box<dyn Any + Send + 'static>;

enum TaskData<F: Future> {
    Idle(Pin<Box<F>>),
    Polling,
    Ready(Result<F::Output, TaskError>),
    Consumed,
}

struct Task<F: Future> {
    state: TaskState,
    joiner: AtomicWaker,
    data: Mutex<TaskData<F>>,
    executor: Arc<Executor>,
}

impl<F> Wake for Task<F>
where
    F: Future + Send + 'static,
    F::Output: Send + 'static,
{
    fn wake(self: Arc<Self>) {
        if self.state.transition_to_schedule() {
            self.schedule();
        }
    }

    fn wake_by_ref(self: &Arc<Self>) {
        if self.state.transition_to_schedule() {
            Arc::clone(self).schedule();
        }
    }
}

impl<F> Task<F>
where
    F: Future + Send + 'static,
    F::Output: Send + 'static,
{
    fn schedule(self: Arc<Self>) {
        let mut task = Some(self);

        let with_thread = Thread::with_current(|thread| {
            let task = task.take().unwrap();
            let runnable: Arc<dyn TaskRunnable> = task;
            thread.executor.schedule(once(runnable), Some(thread));
        });

        if with_thread.is_none() {
            let task = task.take().unwrap();
            let executor = task.executor.clone();
            let runnable: Arc<dyn TaskRunnable> = task;
            executor.schedule(once(runnable), None);
        }
    }
}

pub trait TaskRunnable: Send + Sync {
    fn run(self: Arc<Self>, executor: &Arc<Executor>, thread: &Thread);
}

impl<F> TaskRunnable for Task<F>
where
    F: Future + Send + 'static,
    F::Output: Send + 'static,
{
    fn run(self: Arc<Self>, executor: &Arc<Executor>, thread: &Thread) {
        let shutdown = self.transition_to_running();

        let mut data = self.data.lock();
        let mut future = match mem::replace(&mut *data, TaskData::Polling) {
            TaskData::Idle(future) => future,
            _ => unreachable!(),
        };

        let poll_result = panic::catch_unwind(panic::AssertUnwindSafe(|| {
            let waker = Waker::from(self.clone());
            let mut ctx = Context::from_waker(&waker);
            future.as_mut().poll(&mut ctx)
        }));

        let result = match poll_result {
            Ok(Poll::Ready(result)) => Ok(result),
            Err(error) => Err(error),
            Ok(Poll::Pending) => {
                *data = TaskData::Idle(future);
                mem::drop(data);

                if self.transition_to_idle() {
                    return;
                }

                let runnable: Arc<dyn TaskRunnable> = self;
                executor.schedule(once(runnable), Some(thread));
                return;
            }
        };

        *data = TaskData::Ready(result);
        mem::drop(data);
        self.joiner.wake().map(Waker::wake).unwrap_or(());

        if shutdown {
            executor.shutdown();
        }
    }
}

trait TaskJoinable<T> {
    fn join(&self, waker_ref: &Waker) -> Poll<T>;
    fn consume(&self) -> T;
    fn detach(&self);
}

impl<F: Future> TaskJoinable<F::Output> for Task<F> {
    fn join(&self, waker_ref: &Waker) -> Poll<F::Output> {
        match self.joiner.poll(waker_ref, || {}, || {}) {
            Poll::Ready(_) => Poll::Ready(self.consume()),
            Poll::Pending => Poll::Pending,
        }
    }

    fn consume(&self) -> F::Output {
        match mem::replace(&mut *self.data.lock(), TaskData::Consumed) {
            TaskData::Ready(Ok(result)) => result,
            TaskData::Ready(Err(error)) => panic::resume_unwind(error),
            TaskData::Polling => unreachable!("Polling when consumed"),
            TaskData::Consumed => unreachable!("Consumed when consumed"),
            TaskData::Idle(_) => unreachable!("Idle when consumed"),
        }
    }

    fn detach(&self) {
        mem::drop(self.joiner.detach())
    }
}

pub struct JoinHandle<T> {
    joinable: Option<Arc<dyn TaskJoinable<T>>>,
}

impl<T> JoinHandle<T> {
    pub(crate) fn consume(self) -> T {
        self.joinable
            .as_ref()
            .expect("JoinHandle polled to completion already")
            .consume()
    }
}

impl<T> Drop for JoinHandle<T> {
    fn drop(&mut self) {
        if let Some(joinable) = self.joinable.take() {
            joinable.detach();
        }
    }
}

impl<T> Future for JoinHandle<T> {
    type Output = T;

    fn poll(mut self: Pin<&mut Self>, ctx: &mut Context<'_>) -> Poll<Self::Output> {
        let joinable = self
            .joinable
            .as_ref()
            .expect("JoinHandle polled to completion already");

        let result = match joinable.join(ctx.waker()) {
            Poll::Ready(result) => result,
            Poll::Pending => return Poll::Pending,
        };

        self.joinable = None;
        Poll::Ready(result)
    }
}

pub fn spawn<F>(future: F) -> JoinHandle<F::Output>
where
    F: Future + Send + 'static,
    F::Output: Send + 'static,
{
    Thread::with_current(|thread| {
        let task = Arc::new(Self {
            status: TaskState::from(TaskStatus::Scheduled),
            joiner: AtomicWaker::default(),
            data: Mutex::new(TaskData::Idle(Box::pin(future))),
            executor: executor.clone(),
        });

        let runnable: Arc<dyn TaskRunnable> = task.clone();
        executor.schedule(once(runnable), Some(thread));

        JoinHandle {
            joinable: Some(task),
        }
    })
        .expect("spawn() called outside the runtime")
}

pub(crate) fn block_on<F: Future>(future: F, executor: &Arc<Executor>) -> F::Output {
    
}

pub fn yield_now() -> impl Future<Output = ()> {
    struct YieldNow {
        yielded: bool,
    }

    impl Future for YieldNow {
        type Output = ();

        fn poll(mut self: Pin<&mut Self>, ctx: &mut Context<'_>) -> Poll<Self::Output> {
            if mem::replace(&mut self.yielded, true) {
                return Poll::Ready(());
            }

            ctx.waker().wake_by_ref();
            Poll::Pending
        }
    }

    YieldNow { yielded: false }
}
