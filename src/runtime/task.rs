use super::{context::Context, executor::Executor, waker::AtomicWaker};
use std::{
    any::Any,
    future::Future,
    mem::{drop, replace},
    panic,
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

#[derive(Default)]
pub struct TaskState {
    state: AtomicU8,
}

impl TaskState {
    pub fn transition_to_scheduled(&self) -> bool {
        self.state
            .fetch_update(Ordering::AcqRel, Ordering::Relaxed, |state| match state {
                TASK_IDLE => Some(TASK_SCHEDULED),
                TASK_RUNNING => Some(TASK_NOTIFIED),
                TASK_SCHEDULED | TASK_NOTIFIED => None,
                _ => unreachable!("invalid TaskState"),
            })
            .map(|state| state == TASK_IDLE)
            .unwrap_or(false)
    }

    pub fn transition_to_running_from_scheduled(&self) -> bool {
        self.transition_to_running_from(TASK_SCHEDULED)
    }

    pub fn transition_to_running_from_notified(&self) -> bool {
        self.transition_to_running_from(TASK_NOTIFIED)
    }

    fn transition_to_running_from(&self, ready_state: u8) -> bool {
        self.state.load(Ordering::Acquire) == ready_state && {
            self.state.store(TASK_RUNNING, Ordering::Relaxed);
            true
        }
    }

    pub fn transition_to_idle(&self) -> bool {
        self.state
            .compare_exchange(
                TASK_RUNNING,
                TASK_IDLE,
                Ordering::Release,
                Ordering::Relaxed,
            )
            .is_ok()
    }
}

enum TaskData<F: Future> {
    Polling(Pin<Box<F>>, CoopBudget),
    Ready(F::Output),
    Error(Box<dyn Any + Send + 'static>),
    Joined,
}

pub struct Task<F: Future> {
    state: TaskState,
    data: TryLock<TaskData<F>>,
    waker: AtomicWaker,
    executor: Arc<Executor>,
}

impl<F> Task<F>
where
    F: Future + Send + 'static,
    F::Output: Send + 'static,
{
    pub fn spawn(future: F, executor: Arc<Executor>, context: Option<&Context>) -> Arc<Self> {
        let task = Arc::new(Self {
            state: TaskState::default(),
            data: TryLock::new(TaskData::Polling(Box::pin(future), CoopBudget::default())),
            waker: AtomicWaker::default(),
            executor,
        });

        executor.pool.task_start();
        assert!(task.state.transition_to_scheduled());
        task.executor.schedule(task.clone(), context);

        task
    }

    fn schedule(self: Arc<Self>) {
        match Context::try_current() {
            Some(context) => context.executor.schedule(self, Some(&context)),
            None => self.executor.clone().schedule(self, None),
        }
    }
}

impl<F> Wake for Task<F> {
    fn wake(self: Arc<Self>) {
        if self.state.transition_to_scheduled() {
            self.clone().schedule();
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
        assert!(self.state.transition_to_running_from_scheduled());

        let mut data = self
            .data
            .try_lock()
            .expect("Task data being accessed by multiple threads");

        let poll_result = match &mut *data {
            TaskData::Polling(future, coop_budget) => {
                *context.coop_budget.borrow_mut() = Some(coop_budget.clone());
                let polled = {
                    let waker = Waker::from(self.clone());
                    let mut ctx = PollContext::from_waker(&waker);
                    panic::catch_unwind(panic::AssertUnwindSafe(|| future.as_mut().poll(&mut ctx)))
                };

                *coop_budget = context.coop_budget.borrow_mut().take().unwrap();
                polled
            }
            TaskData::Joined => unreachable!("Task being polled when joined"),
            _ => unreachable!("Task being polled when completed"),
        };

        *data = match poll_result {
            Err(error) => TaskData::Error(error),
            Ok(Poll::Ready(output)) => TaskData::Ready(output),
            Ok(Poll::Pending) => {
                drop(data);

                if self.state.transition_to_idle() {
                    return;
                }

                assert!(self.state.transition_to_running_from_notified());
                context.executor.schedule(self, Some(context));
                return;
            }
        };

        drop(data);
        self.waker.wake();
        context.executor.pool.task_complete();
    }
}

pub trait TaskJoinable<T> {
    fn poll_join(&self, ctx: &mut PollContext<'_>) -> Poll<T>;
}

impl<F: Future> TaskJoinable<F::Output> for Task<F> {
    fn poll_join(&self, ctx: &mut PollContext<'_>) -> Poll<T> {
        if self.waker.poll(ctx).is_pending() {
            return Poll::Pending;
        }

        let data = self
            .data
            .try_lock()
            .expect("Task data being accessed by multiple threads");

        match replace(&mut *data, TaskData::Joined) {
            TaskData::Ready(output) => Poll::Ready(output),
            TaskData::Error(error) => panic::resume_unwind(error),
            TaskData::Joined => unreachable!("Task joined multiple times"),
            TaskData::Polling(_, _) => unreachable!("Task joined while polling"),
        }
    }
}
