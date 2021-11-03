use crate::runtime::scheduler::{
    task::{Task, TaskJoinable},
    thread::Thread,
};
use std::{
    future::Future,
    pin::Pin,
    sync::Arc,
    task::{Context, Poll},
};

pub fn spawn<F>(future: F) -> JoinHandle<F::Output>
where
    F: Future + Send + 'static,
    F::Output: Send + 'static,
{
    Thread::try_with(|thread| {
        let task = Task::spawn(future, &thread.executor, Some(thread));
        JoinHandle {
            joinable: Some(task),
        }
    })
    .expect("spawn() called outside of the runtime")
}

pub struct JoinHandle<T> {
    joinable: Option<Arc<dyn TaskJoinable<T> + Send + Sync>>,
}

impl<T> Future for JoinHandle<T> {
    type Output = T;

    fn poll(mut self: Pin<&mut Self>, ctx: &mut Context<'_>) -> Poll<Self::Output> {
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

        fn poll(mut self: Pin<&mut Self>, ctx: &mut Context<'_>) -> Poll<()> {
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
