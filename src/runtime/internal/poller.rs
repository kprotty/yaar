use super::{context::Context, queue::Runnable};
use crate::net::internal::poller::{PollGuard, PollEvents, Poller as NetPoller};
use crate::time::internal::queue::{Expired, Queue as TimerQueue};
use std::{
    collections::VecDeque,
    mem::replace,
    time::{Duration, Instant},
};

#[derive(Default)]
pub struct Poller {
    expired: Expired,
    events: PollEvents,
    ready: VecDeque<Runnable>,
}

impl Poller {
    pub fn pending(&self) -> bool {
        self.ready.len() > 0
    }

    pub fn inject(&mut self, context: &Context) {
        if self.pending() {
            context.executor.inject(self.ready.drain(..));
        }
    }

    pub fn pop_and_inject(&mut self, context: &Context) -> Option<Runnable> {
        let runnable = self.ready.pop_front();
        self.inject(context);
        runnable
    }

    pub fn poll_timers(
        &mut self,
        context: &Context,
        timer_queue: &TimerQueue,
        now: &mut Option<Instant>,
        timeout: &mut Option<Duration>,
    ) {
        let expires = match timer_queue.expires() {
            Some(expires) => expires,
            None => return,
        };

        if now.is_none() {
            *now = Some(Instant::now());
        }

        let current = timer_queue.since(now.unwrap());
        if current < expires {
            let duration = Duration::from_millis(expires - current);
            *timeout = Some(timeout.map(|t| t.min(duration)).unwrap_or(duration));
            return;
        }

        timer_queue.poll(current, &mut self.expired);
        if !self.expired.pending() {
            return;
        }

        let ready = replace(&mut self.ready, VecDeque::new());
        let intercept = context.intercept.borrow_mut().replace(ready);
        assert!(intercept.is_none());

        self.expired.process();

        let intercept = context.intercept.borrow_mut().take();
        self.ready = intercept.unwrap();
    }

    pub fn try_poll_network<'a, 'b, 'c>(
        &'a mut self,
        context: &'c Context,
        net_poller: &'b NetPoller,
    ) -> Option<PollNetworkGuard<'a, 'b, 'c>> {
        net_poller.try_poll().map(|poll_guard| PollNetworkGuard {
            poller: self,
            net_poller,
            poll_guard,
            context,
        })
    }
}

pub struct PollNetworkGuard<'a, 'b, 'c> {
    poller: &'a mut Poller,
    net_poller: &'b NetPoller,
    poll_guard: PollGuard<'b>,
    context: &'c Context,
}

impl<'a, 'b, 'c> PollNetworkGuard<'a, 'b, 'c> {
    pub fn poll(self, timeout: Option<Duration>) {
        let Self {
            poller,
            net_poller,
            poll_guard,
            context,
        } = self;

        poll_guard.poll(&mut poller.events, timeout);

        let ready = replace(&mut poller.ready, VecDeque::new());
        let intercept = context.intercept.borrow_mut().replace(ready);
        assert!(intercept.is_none());

        poller.events.process(net_poller);

        let intercept = context.intercept.borrow_mut().take();
        poller.ready = intercept.unwrap();
    }
}
