use super::{
    internal::queue::{Delay, Queue as TimerQueue},
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
    Polling((Arc<TimerQueue>, Delay)),
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
                let timer_queue = TimerQueue::with(|queue| queue.clone());

                // A failed schedule means the delay has already expired.
                // Instead of returning immediately, do a yield.
                // The next poll with see State::Polled and return Poll::Ready.
                let delay = match timer_queue.schedule(delay) {
                    Some(delay) => delay,
                    None => {
                        ctx.waker().wake_by_ref();
                        return Poll::Pending;
                    }
                };

                // Poll the AtomicWaker to see if we we're notified by TimerQueue::Entries::process().
                if delay.entry.waker.poll(Some(ctx)).is_ready() {
                    timer_queue.complete(delay);
                    return Poll::Ready(());
                }

                self.state = State::Polling((timer_queue, delay));
                Poll::Pending
            }
            State::Polling((timer_queue, delay)) => {
                // Poll the AtomicWaker to see if we we're notified by TimerQueue::Entries::process().
                if delay.entry.waker.poll(Some(ctx)).is_ready() {
                    timer_queue.complete(delay);
                    return Poll::Ready(());
                }

                // We weren't, go back to sleeping (waker was regitered above)
                self.state = State::Polling((timer_queue, delay));
                Poll::Pending
            }
            State::Polled => {
                // The future was polled to completion
                Poll::Ready(())
            }
        }
    }
}

impl Drop for Sleep {
    fn drop(&mut self) {
        if let State::Polling((timer_queue, delay)) = replace(&mut self.state, State::Polled) {
            // Still need to complete (cancel) the delay if we started but never polled to completion
            timer_queue.complete(delay);
        }
    }
}
