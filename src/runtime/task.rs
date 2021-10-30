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
    task::{ready, Context, Poll, Wake, Waker},
};

type TaskError = Box<dyn Any + Send + 'static>;

enum TaskData<F: Future> {
    Idle(Pin<Box<F>>),
    Polling,
    Ready(Result<F::Output, TaskError>),
    Consumed,
}

struct Task<F: Future> {
    state: AtomicU8,
    joiner: AtomicWaker,
    data: Mutex<TaskData<F>>,
    executor: Arc<Executor>,
}

#[derive(Copy, Clone, Eq, PartialEq, Debug)]
enum TaskStatus {
    Idle = 0,
    Scheduled = 1,
    Running = 2,
    Notified = 3,
}

#[derive(Copy, Clone)]
struct TaskState {
    shutdown: bool,
    status: TaskStatus,
}

impl Into<u8> for TaskState {
    fn into(self) -> u8 {
        ((self.shutdown as u8) << 2) | (self.status as u8)
    }
}

impl From<u8> for TaskState {
    fn from(value: u8) -> Self {
        Self {
            shutdown: value & (1 << 2) != 0,
            status: match value & 0b11 {
                0 => TaskStatus::Idle,
                1 => TaskStatus::Scheduled,
                2 => TaskStatus::Running,
                3 => TaskStatus::Notified,
                _ => unreachable!(),
            },
        }
    }
}

impl<F> Task<F>
where
    F: Future + Send + 'static,
    F::Output: Send,
{
    fn new(future: F, executor: Arc<Executor>, shutdown: bool) -> Self {
        let state = TaskState {
            status: TaskStatus::Scheduled,
            shutdown,
        };

        Self {
            state: AtomicU8::new(state.into()),
            joiner: AtomicWaker::default(),
            data: Mutex::new(TaskData::Idle(Box::pin(future))),
            executor,
        }
    }

    fn transition_to_schedule(&self) -> bool {
        self.state
            .fetch_update(Ordering::Release, Ordering::Relaxed, |state| {
                let mut state = TaskState::from(state);
                state.status = match state.status {
                    TaskStatus::Idle => TaskStatus::Scheduled,
                    TaskStatus::Running => TaskStatus::Notified,
                    _ => return None,
                };
                Some(state.into())
            })
            .map(|state| match TaskState::from(state).status {
                TaskStatus::Idle => true,
                TaskStatus::Running => false,
                _ => unreachable!(),
            })
            .unwrap_or(false)
    }

    fn transition_to_running(&self) -> bool {
        let mut state: TaskState = self.state.load(Ordering::Relaxed).into();
        assert_eq!(state.status, TaskStatus::Scheduled);

        state.status = TaskStatus::Running;
        self.state.store(state.into(), Ordering::Relaxed);
        state.shutdown
    }

    fn transition_to_idle(&self) -> bool {
        let mut state: TaskState = self.state.load(Ordering::Acquire).into();

        if state.status == TaskStatus::Running {
            match self.state.compare_exchange(
                state.into(),
                (TaskState {
                    status: TaskStatus::Idle,
                    shutdown: state.shutdown,
                })
                .into(),
                Ordering::AcqRel,
                Ordering::Acquire,
            ) {
                Ok(_) => return true,
                Err(e) => state = TaskState::from(e),
            }
        }

        assert_eq!(state.status, TaskStatus::Notified);
        state.status = TaskStatus::Scheduled;
        self.state.store(state.into(), Ordering::Relaxed);
        false
    }

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

impl<F> Wake for Task<F>
where
    F: Future + Send + 'static,
    F::Output: Send,
{
    fn wake(self: Arc<Self>) {
        if self.transition_to_schedule() {
            self.schedule();
        }
    }

    fn wake_by_ref(self: &Arc<Self>) {
        if self.transition_to_schedule() {
            Arc::clone(self).schedule();
        }
    }
}

pub trait TaskRunnable: Send + Sync {
    fn run(self: Arc<Self>, executor: &Arc<Executor>, thread: &Thread);
}

impl<F> TaskRunnable for Task<F>
where
    F: Future + Send + 'static,
    F::Output: Send,
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

trait TaskJoinable<T>: Send + Sync {
    fn join(&self, waker_ref: &Waker) -> Poll<T>;
    fn consume(&self) -> T;
}

impl<F> TaskJoinable<F::Output> for Task<F>
where
    F: Future + Send + 'static,
    F::Output: Send,
{
    fn join(&self, waker_ref: &Waker) -> Poll<F::Output> {
        let _ = ready!(self.joiner.poll(waker_ref, || {}));
        Poll::Ready(self.consume())
    }

    fn consume(&self) -> F::Output {
        match mem::replace(&mut *self.data.lock(), TaskData::Consumed) {
            TaskData::Ready(Ok(result)) => result,
            TaskData::Ready(Err(error)) => panic::resume_unwind(error),
            _ => unreachable!(),
            // TaskData::Polling => unreachable!("Polling when consumed"),
            // TaskData::Consumed => unreachable!("Consumed when consumed"),
            // TaskData::Idle(_) => unreachable!("Idle when consumed"),
        }
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

impl<T> Future for JoinHandle<T> {
    type Output = T;

    fn poll(mut self: Pin<&mut Self>, ctx: &mut Context<'_>) -> Poll<Self::Output> {
        let joinable = self
            .joinable
            .as_ref()
            .expect("JoinHandle polled to completion already");

        let result = ready!(joinable.join(ctx.waker()));
        self.joinable = None;
        Poll::Ready(result)
    }
}

pub(crate) fn block_on<F>(future: F, executor: &Arc<Executor>) -> F::Output
where
    F: Future + Send + 'static,
    F::Output: Send + 'static,
{
    let join_handle = spawn_with(future, executor, None, true);
    join_handle.consume()
}

pub fn spawn<F>(future: F) -> JoinHandle<F::Output>
where
    F: Future + Send + 'static,
    F::Output: Send + 'static,
{
    Thread::with_current(|thread| spawn_with(future, &thread.executor, Some(thread), false))
        .expect("spawn() called outside the runtime")
}

fn spawn_with<F>(
    future: F,
    executor: &Arc<Executor>,
    thread: Option<&Thread>,
    shutdown: bool,
) -> JoinHandle<F::Output>
where
    F: Future + Send + 'static,
    F::Output: Send + 'static,
{
    let task = Arc::new(Task::new(future, executor.clone(), shutdown));

    let runnable: Arc<dyn TaskRunnable> = task.clone();
    executor.schedule(once(runnable), thread);

    JoinHandle {
        joinable: Some(task),
    }
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
