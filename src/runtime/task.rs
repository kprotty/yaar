use super::executor::Executor;
use crate::sync::waker::AtomicWaker;
use parking_lot::Mutex;
use std::{
    any::Any,
    future::Future,
    mem, panic,
    pin::Pin,
    sync::{
        atomic::{AtomicU8, Ordering},
        Arc,
    },
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

impl<F> Task<F>
where
    F: Future + Send + 'static,
    F::Output: Send,
{
    const TASK_IDLE: u8 = 0;
    const TASK_SCHEDULED: u8 = 1;
    const TASK_RUNNING: u8 = 2;
    const TASK_NOTIFIED: u8 = 3;

    fn new(future: F, executor: Arc<Executor>) -> Self {
        Self {
            state: AtomicU8::new(Self::TASK_IDLE),
            joiner: AtomicWaker::default(),
            data: Mutex::new(TaskData::Idle(Box::pin(future))),
            executor,
        }
    }

    fn transition_to_schedule(&self) -> bool {
        self.state
            .fetch_update(Ordering::Release, Ordering::Relaxed, |state| match state {
                Self::TASK_IDLE => Some(Self::TASK_SCHEDULED),
                Self::TASK_SCHEDULED => None,
                Self::TASK_RUNNING => Some(Self::TASK_NOTIFIED),
                Self::TASK_NOTIFIED => None,
                _ => unreachable!(),
            })
            .map(|state| match state {
                Self::TASK_IDLE => true,
                Self::TASK_RUNNING => false,
                _ => unreachable!(),
            })
            .unwrap_or(false)
    }

    fn transition_to_running(&self) {
        assert_eq!(self.state.load(Ordering::Relaxed), Self::TASK_SCHEDULED);
        self.state.store(Self::TASK_RUNNING, Ordering::Relaxed);
    }

    fn transition_to_idle(&self) -> bool {
        match self.state.compare_exchange(
            Self::TASK_RUNNING,
            Self::TASK_IDLE,
            Ordering::AcqRel,
            Ordering::Acquire,
        ) {
            Ok(_) => true,
            Err(Self::TASK_NOTIFIED) => {
                self.state.store(Self::TASK_SCHEDULED, Ordering::Relaxed);
                false
            }
            Err(_) => unreachable!(),
        }
    }

    fn schedule(self: Arc<Self>) {
        let mut task = Some(self);
        let with_worker = Executor::with_worker(|executor, worker_index| {
            executor.schedule(task.take().unwrap(), Some(worker_index))
        });

        if with_worker.is_none() {
            let task = task.take().unwrap();
            let executor = task.executor.clone();
            executor.schedule(task, None);
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
    fn run(self: Arc<Self>, executor: &Arc<Executor>, worker_index: usize);
}

impl<F> TaskRunnable for Task<F>
where
    F: Future + Send + 'static,
    F::Output: Send,
{
    fn run(self: Arc<Self>, executor: &Arc<Executor>, worker_index: usize) {
        self.transition_to_running();

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
                } else {
                    return executor.schedule(self, Some(worker_index));
                }
            }
        };

        *data = TaskData::Ready(result);
        mem::drop(data);
        self.joiner.wake().map(Waker::wake).unwrap_or(())
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
        }
    }
}

pub struct JoinHandle<T> {
    joinable: Option<Arc<dyn TaskJoinable<T>>>,
}

impl<T> JoinHandle<T> {
    pub(crate) fn consume(mut self) -> T {
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

pub(crate) fn run<F>(future: F, executor: &Arc<Executor>, worker_index: usize) -> F::Output
where
    F: Future + Send + 'static,
    F::Output: Send + 'static,
{
    spawn_with(future, executor, worker_index).consume()
}

pub fn spawn<F>(future: F) -> JoinHandle<F::Output>
where
    F: Future + Send + 'static,
    F::Output: Send + 'static,
{
    Executor::with_worker(|executor, worker_index| spawn_with(future, executor, worker_index))
        .expect("spawn() called outside the runtime")
}

fn spawn_with<F>(future: F, executor: &Arc<Executor>, worker_index: usize) -> JoinHandle<F::Output>
where
    F: Future + Send + 'static,
    F::Output: Send + 'static,
{
    let task = Arc::new(Task::new(future, executor.clone()));
    executor.schedule(task.clone(), Some(worker_index));
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
