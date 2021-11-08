use super::{error::Elapsed, sleep, sleep_until, Duration, Instant, Sleep};
use pin_project_lite::pin_project;
use std::{
    future::Future,
    pin::Pin,
    task::{Context, Poll},
};

pub fn timeout<F: Future>(duration: Duration, future: F) -> Timeout<F> {
    Timeout {
        sleeper: sleep(duration),
        future,
    }
}

pub fn timeout_at<F: Future>(instant: Instant, future: F) -> Timeout<F> {
    Timeout {
        sleeper: sleep_until(instant),
        future,
    }
}

pin_project! {
    pub struct Timeout<F> {
        #[pin]
        sleeper: Sleep,
        #[pin]
        future: F,
    }
}

impl<F> AsRef<F> for Timeout<F> {
    fn as_ref(&self) -> &F {
        &self.future
    }
}

impl<F> AsMut<F> for Timeout<F> {
    fn as_mut(&mut self) -> &mut F {
        &mut self.future
    }
}

impl<F> Timeout<F> {
    pub fn into_inner(self) -> F {
        self.future
    }
}

impl<F: Future> Future for Timeout<F> {
    type Output = Result<F::Output, Elapsed>;

    fn poll(self: Pin<&mut Self>, ctx: &mut Context<'_>) -> Poll<Self::Output> {
        let mut_self = self.project();

        if let Poll::Ready(result) = mut_self.future.poll(ctx) {
            return Poll::Ready(Ok(result));
        }

        if let Poll::Ready(()) = mut_self.sleeper.poll(ctx) {
            return Poll::Ready(Err(Elapsed::new()));
        }

        Poll::Pending
    }
}
