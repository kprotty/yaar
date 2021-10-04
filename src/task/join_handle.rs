use crate::runtime::task::{Task, TaskVTable};
use std::{
    future::Future,
    marker::PhantomData,
    mem::MaybeUninit,
    pin::Pin,
    task::{Context, Poll, Waker},
};

pub struct JoinHandle<T> {
    task: Option<NonNull<Task>>,
    _phantom: PhantomData<T>,
}

unsafe impl<T: Send> Send for JoinHandle<T> {}

impl<T> Future for JoinHandle<T> {
    type Output = T;

    fn poll(mut self: Pin<&mut Self>, ctx: &mut Context<'_>) -> Poll<Self::Output> {
        let task = self
            .take_task()
            .expect("JoinHandle polled after completion");

        let mut output = MaybeUninit::<T>::uninit();
        let output_ptr = NonNull::new(output.as_mut_ptr()).unwrap();

        match (task.vtable.join_fn)(task, ctx.waker(), output_ptr.cast()) {
            Poll::Ready(()) => Poll::Ready(unsafe { output.assume_init() }),
            Poll::Pending => {
                self.task = Some(task);
                Poll::Pending
            }
        }
    }
}

impl<T> Drop for JoinHandle<T> {
    fn drop(&mut self) {
        if let Some(task) = self.take_task() {
            (task.vtable.detach_fn)(task);
        }
    }
}

impl<T> JoinHandle<T> {
    pub(crate) unsafe fn new(task: NonNull<Task>) -> Self {
        Self {
            task: Some(task),
            _phantom: PhantomData,
        }
    }

    fn take_task(&mut self) -> Option<Pin<&Task>> {
        self.task
            .take()
            .map(|task| unsafe { Pin::new_unchecked(&*task.as_ptr()) })
    }

    pub(crate) fn consume(mut self) -> T {
        let task = self
            .take_task()
            .expect("JoinHandle consumed after being polled to completion");

        let mut output = MaybeUninit::<T>::uninit();
        let output_ptr = NonNull::new(output.as_mut_ptr()).unwrap();
        (task.vtable.consume_fn)(task, output_ptr.cast());
        unsafe { output.assume_init() }
    }
}
