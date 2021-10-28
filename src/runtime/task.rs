use crate::{
    runtime::{executor::ExecutorRef, thread::Thread},
    sync::waker::AtomicWaker,
};
use parking_lot::Mutex;
use std::{
    any::Any,
    future::Future,
    mem, panic,
    pin::Pin,
    sync::atomic::{AtomicU8, Ordering},
    sync::Arc,
    task::{Context, Poll, Wake, Waker},
};

const TASK_IDLE: u8 = 0;
const TASK_SCHEDULED: u8 = 1;
const TASK_RUNNING: u8 = 2;
const TASK_NOTIFIED: u8 = 3;

type TaskError = Box<dyn Any + Send + 'static>;

enum TaskData<F: Future> {
    Idle(Pin<Box<F>>),
    Polling,
    Ready(Result<F::Output, TaskError>),
    Consumed,
}

pub struct Task<F: Future> {
    state: AtomicU8,
    joiner: AtomicWaker,
    data: Mutex<TaskData<F>>,
    executor_ref: Arc<ExecutorRef>,
}

impl<F> Wake for Task<F>
where
    F: Future + Send + 'static,
    F::Output: Send,
{
    fn wake(self: Arc<Self>) {
        if self.try_schedule_wake() {
            self.schedule_wake()
        }
    }

    fn wake_by_ref(self: &Arc<Self>) {
        if self.try_schedule_wake() {
            Arc::clone(self).schedule_wake()
        }
    }
}

impl<F> Task<F>
where
    F: Future + Send + 'static,
    F::Output: Send,
{
    fn try_schedule_wake(&self) -> bool {
        self.state
            .fetch_update(Ordering::Release, Ordering::Relaxed, |state| match state {
                TASK_IDLE => Some(TASK_SCHEDULED),
                TASK_SCHEDULED => None,
                TASK_RUNNING => Some(TASK_NOTIFIED),
                TASK_NOTIFIED => None,
                _ => unreachable!(),
            })
            .map(|state| match state {
                TASK_IDLE => true,
                TASK_RUNNING => false,
                _ => unreachable!(),
            })
            .unwrap_or(false)
    }

    fn schedule_wake(self: Arc<Self>) {
        let mut task = Some(self);
        let wake_result = Thread::with_worker(|executor_ref, worker_index| {
            executor_ref.schedule(TaskScheduled::Awoken {
                task: task.take().unwrap(),
                worker_index,
            })
        });

        if wake_result.is_none() {
            let task = task.take().unwrap();
            let executor_ref = task.executor_ref.clone();
            executor_ref.schedule(TaskScheduled::Injected { task });
        }
    }
}

pub enum TaskScheduled {
    Spawned {
        task: Arc<dyn TaskPoll>,
        worker_index: usize,
    },
    Yielded {
        task: Arc<dyn TaskPoll>,
        worker_index: usize,
    },
    Awoken {
        task: Arc<dyn TaskPoll>,
        worker_index: usize,
    },
    Injected {
        task: Arc<dyn TaskPoll>,
    },
}

pub trait TaskPoll: Send + Sync {
    fn poll(self: Arc<Self>, executor_ref: &Arc<ExecutorRef>, worker_index: usize);
}

impl<F> TaskPoll for Task<F>
where
    F: Future + Send + 'static,
    F::Output: Send,
{
    fn poll(self: Arc<Self>, executor_ref: &Arc<ExecutorRef>, worker_index: usize) {
        assert_eq!(self.state.load(Ordering::Relaxed), TASK_SCHEDULED);
        self.state.store(TASK_RUNNING, Ordering::Relaxed);

        let mut data = self.data.lock();
        let mut future = match mem::replace(&mut *data, TaskData::Polling) {
            TaskData::Idle(future) => future,
            _ => unreachable!(),
        };

        let poll_result = panic::catch_unwind(panic::AssertUnwindSafe(|| {
            let waker = Waker::from(Arc::clone(&self));
            let mut context = Context::from_waker(&waker);
            future.as_mut().poll(&mut context)
        }));

        let result = match poll_result {
            Err(error) => Err(error),
            Ok(Poll::Ready(result)) => Ok(result),
            Ok(Poll::Pending) => {
                match mem::replace(&mut *data, TaskData::Idle(future)) {
                    TaskData::Polling => {}
                    _ => unreachable!(),
                }

                drop(data);
                return match self.state.compare_exchange(
                    TASK_RUNNING,
                    TASK_IDLE,
                    Ordering::AcqRel,
                    Ordering::Acquire,
                ) {
                    Ok(_) => {}
                    Err(TASK_NOTIFIED) => executor_ref.schedule(TaskScheduled::Yielded {
                        task: self,
                        worker_index,
                    }),
                    Err(_) => unreachable!(),
                };
            }
        };

        match mem::replace(&mut *data, TaskData::Ready(result)) {
            TaskData::Polling => {}
            _ => unreachable!(),
        }

        drop(data);
        self.joiner.wake().map(Waker::wake).unwrap_or(())
    }
}

trait TaskJoin<T>: Send + Sync {
    fn join(&self, waker_ref: &Waker) -> Poll<T>;
    fn consume(&self) -> T;
}

impl<F> TaskJoin<F::Output> for Task<F>
where
    F: Future + Send,
    F::Output: Send,
{
    fn join(&self, waker_ref: &Waker) -> Poll<F::Output> {
        match self.joiner.poll(waker_ref, || {}) {
            Poll::Ready(_) => Poll::Ready(self.consume()),
            Poll::Pending => Poll::Pending,
        }
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
    task: Option<Arc<dyn TaskJoin<T>>>,
}

impl<T> JoinHandle<T> {
    pub(crate) fn consume(mut self) -> T {
        self.task
            .take()
            .expect("JoinHandle consumed after being polled to completion")
            .consume()
    }
}

impl<T> Future for JoinHandle<T> {
    type Output = T;

    fn poll(mut self: Pin<&mut Self>, ctx: &mut Context<'_>) -> Poll<Self::Output> {
        let task = self
            .task
            .take()
            .expect("JoinHandle polled after completion");

        if let Poll::Ready(result) = task.join(ctx.waker()) {
            return Poll::Ready(result);
        }

        self.task = Some(task);
        Poll::Pending
    }
}

pub fn spawn<F>(future: F) -> JoinHandle<F::Output>
where
    F: Future + Send + 'static,
    F::Output: Send + 'static,
{
    Thread::with_worker(|executor_ref, worker_index| {
        Task::spawn(executor_ref, worker_index, future)
    })
    .expect("spawn() was called outside of the runtime")
}

trait TaskSpawn<T>: TaskPoll + TaskJoin<T> {}

impl<F> TaskSpawn<F::Output> for Task<F>
where
    F: Future + Send + 'static,
    F::Output: Send,
{
}

impl<F> Task<F>
where
    F: Future + Send + 'static,
    F::Output: Send,
{
    fn spawn(
        executor_ref: &Arc<ExecutorRef>,
        worker_index: usize,
        future: F,
    ) -> JoinHandle<F::Output> {
        let task: Arc<Self> = Arc::new(Self {
            state: AtomicU8::new(TASK_SCHEDULED),
            joiner: AtomicWaker::default(),
            data: Mutex::new(TaskData::Idle(Box::pin(future))),
            executor_ref: Arc::clone(executor_ref),
        });

        executor_ref.schedule(TaskScheduled::Spawned {
            task: task.clone(),
            worker_index,
        });

        JoinHandle { task: Some(task) }
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
