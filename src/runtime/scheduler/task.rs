use super::{executor::Executor, thread::Thread};
use crate::sync::AtomicWaker;
use std::{
    any::Any,
    future::Future,
    mem, panic,
    pin::Pin,
    sync::atomic::{AtomicBool, AtomicU8, Ordering},
    sync::Arc,
    task::{Context, Poll, Wake, Waker},
    thread,
};
use try_lock::TryLock;

const TASK_SCHEDULED: u8 = 0;
const TASK_IDLE: u8 = 1;
const TASK_RUNNING: u8 = 2;
const TASK_NOTIFIED: u8 = 3;

#[derive(Default)]
struct TaskState {
    state: AtomicU8,
}

impl TaskState {
    fn transition_to_scheduled(&self) -> bool {
        self.state
            .fetch_update(Ordering::Acquire, Ordering::Relaxed, |state| match state {
                TASK_IDLE => Some(TASK_SCHEDULED),
                TASK_SCHEDULED => None,
                TASK_RUNNING => Some(TASK_NOTIFIED),
                TASK_NOTIFIED => None,
                _ => unreachable!("invalid TaskState"),
            })
            .map(|state| match state {
                TASK_IDLE => true,
                TASK_NOTIFIED => false,
                _ => unreachable!(),
            })
            .unwrap_or(false)
    }

    fn transition_to_running(&self) {
        let state = self.state.load(Ordering::Acquire);
        assert_eq!(state, TASK_SCHEDULED);

        let new_state = TASK_RUNNING;
        self.state.store(new_state, Ordering::Relaxed);
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
            Err(_) => unreachable!("invalid TaskState"),
        }
    }

    fn transition_from_notified(&self) {
        let state = self.state.load(Ordering::Acquire);
        assert_eq!(state, TASK_NOTIFIED);

        let new_state = TASK_SCHEDULED;
        self.state.store(new_state, Ordering::Relaxed);
    }
}

type TaskError = Box<dyn Any + Send + 'static>;

enum TaskData<F: Future> {
    Idle(Pin<Box<F>>),
    Polling,
    Ready(Result<F::Output, TaskError>),
    Joined,
}

struct Task<F: Future> {
    state: TaskState,
    waker: AtomicWaker,
    data: TryLock<TaskData<F>>,
    executor: Arc<Executor>,
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

impl<F> Task<F>
where
    F: Future + Send + 'static,
    F::Output: Send + 'static,
{
    fn schedule(self: Arc<Self>) {
        let mut task = Some(self);

        let with_thread = Thread::try_with(|thread| {
            let task = task.take().unwrap();
            thread.executor.schedule(task, Some(thread));
        });

        if with_thread.is_none() {
            let task = task.take().unwrap();
            let executor = task.executor.clone();
            executor.schedule(task, None);
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
            Err(error) => Err(error),
            Ok(Poll::Ready(result)) => Ok(result),
            Ok(Poll::Pending) => {
                *data = TaskData::Idle(future);
                mem::drop(data);

                if self.state.transition_to_idle() {
                    return;
                }

                self.state.transition_from_notified();
                thread.executor.yield_now(self, thread);
                return;
            }
        };

        *data = TaskData::Ready(result);
        mem::drop(data);
        self.waker.wake().map(Waker::wake).unwrap_or(());
    }
}

trait TaskJoinable<T> {
    fn poll_join(&self, ctx: &mut Context<'_>) -> Poll<T>;
}

impl<F: Future> TaskJoinable<F::Output> for Task<F> {
    fn poll_join(&self, ctx: &mut Context<'_>) -> Poll<F::Output> {
        match self.waker.poll(Some(ctx.waker()), || {}, || {}) {
            Poll::Ready(_) => {}
            Poll::Pending => return Poll::Pending,
        }

        match mem::replace(&mut *self.data.try_lock().unwrap(), TaskData::Joined) {
            TaskData::Ready(Ok(result)) => Poll::Ready(result),
            TaskData::Ready(Err(error)) => panic::resume_unwind(error),
            _ => unreachable!(),
        }
    }
}

pub struct JoinHandle<T> {
    joinable: Option<Arc<dyn TaskJoinable<T>>>,
}

impl<T> Future for JoinHandle<T> {
    type Output = T;

    fn poll(mut self: Pin<&mut Self>, ctx: &mut Context<'_>) -> Poll<Self::Output> {
        let joinable = self
            .joinable
            .take()
            .expect("JoinHandle polled after completion");

        match joinable.poll_join(ctx) {
            Poll::Ready(result) => Poll::Ready(result),
            Poll::Pending => {
                self.joinable = Some(joinable);
                Poll::Pending
            }
        }
    }
}

pub fn spawn<F>(
    future: F,
    executor: Arc<Executor>,
    thread: Option<&Thread>,
) -> JoinHandle<F::Output>
where
    F: Future + Send + 'static,
    F::Output: Send + 'static,
{
    let task = Arc::new(Task {
        state: TaskState::default(),
        waker: AtomicWaker::default(),
        data: TryLock::new(TaskData::Idle(Box::pin(future))),
        executor: executor.clone(),
    });

    let runnable: Arc<dyn TaskRunnable> = task.clone();
    executor.schedule(runnable, thread);

    JoinHandle {
        joinable: Some(task),
    }
}

pub fn block_on<F: Future>(future: F, executor: Arc<Executor>) -> F::Output {
    struct Blocker {
        state: TaskState,
        notified: AtomicBool,
        thread: thread::Thread,
        executor: Arc<Executor>,
    }

    impl Wake for Blocker {
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

    impl Blocker {
        fn schedule(self: Arc<Self>) {
            let mut blocker = Some(self);

            let with_thread = Thread::try_with(|thread| {
                let blocker = blocker.take().unwrap();
                thread.executor.schedule(blocker, Some(thread));
            });

            if with_thread.is_none() {
                let blocker = blocker.take().unwrap();
                let executor = blocker.executor.clone();
                executor.schedule(blocker, None);
            }
        }
    }

    impl TaskRunnable for Blocker {
        fn run(self: Arc<Self>, _thread: &Thread) {
            self.notified.store(true, Ordering::Release);
            self.thread.unpark();
        }
    }

    let blocker = Arc::new(Blocker {
        state: TaskState::default(),
        notified: AtomicBool::new(false),
        thread: thread::current(),
        executor,
    });

    let waker = Waker::from(blocker.clone());
    let mut ctx = Context::from_waker(&waker);
    pin_utils::pin_mut!(future);

    loop {
        blocker.state.transition_to_running();
        blocker.notified.store(false, Ordering::Relaxed);

        if let Poll::Ready(result) = future.as_mut().poll(&mut ctx) {
            return result;
        }

        if !blocker.state.transition_to_idle() {
            blocker.state.transition_from_notified();
            continue;
        }

        while !blocker.notified.load(Ordering::Acquire) {
            thread::park();
        }
    }
}
