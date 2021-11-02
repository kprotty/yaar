use crate::runtime::scheduler::{task, Thread};
use std::{
    future::Future,
    pin::Pin,
    task::{Context, Poll},
};

pub use task::JoinHandle;

pub fn spawn<F>(future: F) -> JoinHandle<F::Output>
where
    F: Future + Send + 'static,
    F::Output: Send + 'static,
{
    Thread::try_with(|thread| {
        let executor = thread.executor.clone();
        task::spawn(future, executor, Some(thread))
    })
    .expect("spawn() called outside the runtime context")
}

pub async fn yield_now() {
    struct YieldFuture {
        yielded: bool,
    }

    impl Future for YieldFuture {
        type Output = ();

        fn poll(mut self: Pin<&mut Self>, ctx: &mut Context<'_>) -> Poll<Self::Output> {
            if self.yielded {
                return Poll::Ready(());
            }

            self.yielded = true;
            ctx.waker().wake_by_ref();
            return Poll::Pending;
        }
    }

    YieldFuture { yielded: false }.await
}
