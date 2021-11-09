use crate::runtime::internal::{
    context::Context,
    task::{Task, TaskJoinable},
};
use std::{
    future::Future,
    pin::Pin,
    sync::Arc,
    task::{Context as PollContext, Poll},
};

pub fn spawn<F>(future: F) -> JoinHandle<F::Output>
where
    F: Future + Send + 'static,
    F::Output: Send + 'static,
{
    let context_ref = Context::current();
    let context = context_ref.as_ref();

    let task = Task::spawn(future, &context.executor, Some(context));
    JoinHandle {
        joinable: Some(task),
    }
}

pub struct JoinHandle<T> {
    pub(crate) joinable: Option<Arc<dyn TaskJoinable<T> + Send + Sync>>,
}

impl<T> Future for JoinHandle<T> {
    type Output = T;

    fn poll(mut self: Pin<&mut Self>, ctx: &mut PollContext<'_>) -> Poll<Self::Output> {
        let joinable = self
            .joinable
            .as_ref()
            .expect("JoinHandle polled after completion");

        if let Poll::Ready(result) = joinable.poll_join(ctx) {
            self.joinable = None;
            return Poll::Ready(result);
        }

        Poll::Pending
    }
}

pub async fn yield_now() {
    struct YieldFuture {
        yielded: bool,
    }

    impl Future for YieldFuture {
        type Output = ();

        fn poll(mut self: Pin<&mut Self>, ctx: &mut PollContext<'_>) -> Poll<()> {
            if self.yielded {
                return Poll::Ready(());
            }

            self.yielded = true;
            ctx.waker().wake_by_ref();
            Poll::Pending
        }
    }

    YieldFuture { yielded: false }.await
}
