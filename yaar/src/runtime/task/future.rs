use super::{Task, super::with_executor};
use core::{
    future::Future,
    task::{Poll, Context, Waker, RawWaker, RawWakerVTable},
};

pub struct TaskFuture<F: Future> {
    future: F,
    task: Task,
    waker: Option<Waker>,
}

impl<F: Future> Future for Task<F> {
    type Output = F::Output;

    fn poll(self: Pin<&mut Self>, ctx: &mut Context<'_>) -> Poll<Self::Output> {
        let mut_self = unsafe { self.get_unchecked_mut() };
        mut_self.waker = Some(ctx.waker().clone());
        match mut_self.resume() {
            Poll::Pending => Poll::Pending,
            Poll::Ready(output) => {
                core::mem::drop(mut_self.waker.take().unwrap());
                Poll::Ready(output)
            }
        }
    }
}

impl<F: Future> TaskFuture<F> {
    pub const fn new(future: F) -> Self {
        Self {
            future,
            task: Task::new(|task| unsafe {
                let mut_self = &mut *field_parent_ptr!(Self, "task", task);
                let _ = self.resume();
            })
        }
    }

    pub fn resume(&mut self) -> Poll<F::Output> {
        const WAKE_FN: unsafe (ptr: *const ()) = |ptr| unsafe {
            let task = &*(ptr as *const Task)
            with_executor(|e| e.schedule(task))
                .expect("A Task was resumed without being in the context of an executor");
        };

        const VTABLE: RawWakerVTable = RawWakerVTable::new(
            |ptr| RawWaker::new(ptr),
            WAKE_FN,
            WAKE_FN,
            |_| {},
        );

        let waker = self.waker.take().unwrap_or_else(|| {
            let ptr = &self.task as *const Task as usize;
            Waker::from_raw(RawWaker::new(ptr, &VTABLE)
        });
        let pinned = unsafe { Pin::new_unchecked(&mut self.future) };
        pinned.poll(&mut Context::from_waker(&waker))
    }
}