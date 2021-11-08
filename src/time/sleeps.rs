use super::{
    queue::{Delay, DelayQueue},
    Duration, Instant,
};
use std::{
    future::Future,
    mem::replace,
    pin::Pin,
    sync::Arc,
    task::{Context, Poll},
};

pub fn sleep(duration: Duration) -> Sleep {
    Sleep::new(Ok(duration))
}

pub fn sleep_until(instant: Instant) -> Sleep {
    Sleep::new(Err(instant))
}

enum State {
    Start(Result<Duration, Instant>),
    Polling((Arc<DelayQueue>, Delay)),
    Polled,
}

pub struct Sleep {
    state: State,
}

impl Sleep {
    fn new(delay: Result<Duration, Instant>) -> Self {
        Self {
            state: State::Start(delay),
        }
    }
}

impl Future for Sleep {
    type Output = ();

    fn poll(mut self: Pin<&mut Self>, ctx: &mut Context<'_>) -> Poll<()> {
        match replace(&mut self.state, State::Polled) {
            State::Start(delay) => {
                let delay_queue = DelayQueue::with(|queue| queue.clone());
                let delay = match delay_queue.schedule(delay) {
                    Some(delay) => delay,
                    None => return Poll::Ready(()),
                };

                if delay.entry.poll(Some(ctx)).is_ready() {
                    delay_queue.complete(delay, false);
                    return Poll::Ready(());
                }

                self.state = State::Polling((delay_queue, delay));
                Poll::Pending
            }
            State::Polling((delay_queue, delay)) => {
                if delay.entry.poll(Some(ctx)).is_ready() {
                    delay_queue.complete(delay, false);
                    return Poll::Ready(());
                }

                self.state = State::Polling((delay_queue, delay));
                Poll::Pending
            }
            State::Polled => unreachable!("Sleep future polled after completion"),
        }
    }
}

impl Drop for Sleep {
    fn drop(&mut self) {
        if let State::Polling((delay_queue, delay)) = replace(&mut self.state, State::Polled) {
            delay_queue.complete(delay, true);
        }
    }
}
