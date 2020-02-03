use super::{super::with_executor, Task};
use crate::field_parent_ptr;
use core::{
    future::Future,
    pin::Pin,
    task::{Context, Poll, RawWaker, RawWakerVTable, Waker},
};

pub struct TaskFuture<F: Future> {
    future: F,
    task: Task,
}

impl<F: Future> TaskFuture<F> {
    pub fn new(future: F) -> Self {
        Self {
            future,
            task: Task::new(|task| unsafe {
                let mut_self = &mut *field_parent_ptr!(Self, task, task);
                let _ = mut_self.resume();
            }),
        }
    }

    /// Polls the inner future using a stack-allocated waker which re-schedules
    /// the inner task to re-poll the future on the current executor.
    pub fn resume(&mut self) -> Poll<F::Output> {
        const WAKE_FN: unsafe fn(ptr: *const ()) = |ptr| unsafe {
            let task = &*(ptr as *const Task);
            with_executor(|e| e.schedule(task)).unwrap_or_else(|| task.resume())
        };

        const VTABLE: RawWakerVTable =
            RawWakerVTable::new(|ptr| RawWaker::new(ptr, &VTABLE), WAKE_FN, WAKE_FN, |_| {});

        unsafe {
            let ptr = &self.task as *const Task as *const ();
            let waker = Waker::from_raw(RawWaker::new(ptr, &VTABLE));
            let pinned = Pin::new_unchecked(&mut self.future);
            pinned.poll(&mut Context::from_waker(&waker))
        }
    }
}
