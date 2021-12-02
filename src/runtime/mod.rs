mod context;
mod executor;
mod idle;
mod parker;
mod queue;
mod random;
mod task;
mod waker;
mod worker;

use self::{
    context::Context,
    task::{Task, TaskJoinable},
    worker::Worker,
};
use std::{
    future::Future,
    pin::Pin,
    sync::Arc,
    task::{Context as PollContext, Poll},
};

pub fn block_on<F: Future>(future: F) -> F::Output {
    #[cfg(feature = "pin-utils")]
    pin_utils::pin_mut!(future);

    #[cfg(not(feature = "pin-utils"))]
    let future = Box::pin(future);

    Worker::block_on(None, future)
}

pub fn spawn<F>(future: F) -> JoinHandle<F::Output>
where
    F: Future + Send + 'static,
    F::Output: Send + 'static,
{
    let task = Task::spawn(future, Context::try_current().as_ref());
    JoinHandle { task: Some(task) }
}

pub struct JoinHandle<T> {
    task: Option<Arc<dyn TaskJoinable<T>>>,
}

impl<T> Future for JoinHandle<T> {
    type Output = T;

    fn poll(mut self: Pin<&mut Self>, ctx: &mut PollContext<'_>) -> Poll<T> {
        let task = self
            .task
            .as_mut()
            .expect("JoinHandle polled after completion");

        let output = match task.poll_join(ctx) {
            Poll::Pending => return Poll::Pending,
            Poll::Ready(output) => output,
        };

        self.task = None;
        Poll::Ready(output)
    }
}
