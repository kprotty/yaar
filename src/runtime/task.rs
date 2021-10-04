use super::{super::task::JoinHandle, container_of::container_of, queue::Node, waker::Waker};
use std::{
    cell::UnsafeCell,
    future::Future,
    pin::Pin,
    ptr::NonNull,
    sync::atomic::{AtomicUsize, Ordering},
    task::{Poll, Waker},
};

pub(super) struct Task {
    node: Node,
    vtable: &'static TaskVTable,
}

struct TaskVTable {
    pub(super) poll_fn: fn(task: Pin<&Task>),
    clone_fn: fn(task: Pin<&Task>),
    drop_fn: fn(task: Pin<&Task>),
    wake_fn: fn(task: Pin<&Task>, wake_by_ref: bool),
    pub(crate) join_fn: fn(task: Pin<&Task>, waker: &Waker, output: NonNull<()>) -> Poll<()>,
    pub(crate) detach_fn: fn(task: Pin<&Task>),
    pub(crate) consume_fn: fn(task: Pin<&Task>, output: NonNull<()>),
}

#[derive(Copy, Clone, Debug, Eq, PartialEq)]
enum TaskStatus {
    Idle = 0,
    Running = 1,
    Notified = 2,
    Completed = 3,
}

impl Into<usize> for TaskStatus {
    fn into(self) -> usize {
        self as usize
    }
}

impl From<usize> for TaskStatus {
    fn from(value: usize) -> Self {
        [Self::Idle, Self::Running, Self::Notified, Self::Completed][value]
    }
}

#[derive(Copy, Clone)]
struct TaskState {
    pool: Option<NonNull<Pool>>,
    status: TaskStatus,
}

impl Into<usize> for TaskState {
    fn into(self) -> Self {
        let pool_ptr = self.pool.map(|p| p.as_ptr() as usize).unwrap_or(0);
        assert_eq!(pool_ptr & 0b11, 0);
        pool_ptr | self.status.into()
    }
}

impl From<usize> for TaskState {
    fn from(value: usize) -> Self {
        Self {
            pool: NonNull::new((value & !0b11) as *mut Pool),
            status: TaskStatus::from(value & 0b11),
        }
    }
}

enum TaskData<F: Future> {
    Polling(F),
    Ready(F::Output),
    Consumed,
}

pub(crate) struct TaskFuture<F: Future> {
    task: Task,
    waker: AtomicWaker,
    state: AtomicUsize,
    data: UnsafeCell<TaskData<F>>,
}

impl<F: Future> TaskState<F> {
    const TASK_VTABLE: TaskVTable = TaskVTable {
        poll_fn: Self::on_poll,
        clone_fn: Self::on_clone,
        drop_fn: Self::on_drop,
        wake_fn: Self::on_wake,
        join_fn: Self::on_join,
        detach_fn: Self::on_detach,
        consume_fn: Self::on_consume,
    };

    pub(crate) fn spawn(worker_ref: &WorkerRef, future: F) -> JoinHandle<F::Output> {}

    fn on_poll(task: Pin<&Task>) {}

    fn on_clone(task: Pin<&Task>) {}

    fn on_drop(task: Pin<&Task>) {}

    fn on_wake(task: Pin<&Task>, wake_by_ref: bool) {}

    fn on_join(task: Pin<&Task>, waker: &Waker, output: NonNull<()>) -> Poll<()> {}

    fn on_detach(task: Pin<&Task>) {}

    fn on_consume(task: Pin<&Task>, output: NonNull<()>) {}
}
