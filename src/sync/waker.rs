use parking_lot::Mutex;
use std::{
    mem,
    task::{Poll, Waker},
};

enum State {
    Empty,
    Waiting(Waker),
    Notified(usize),
}

pub struct AtomicWaker {
    state: Mutex<State>,
}

impl Default for AtomicWaker {
    fn default() -> Self {
        Self {
            state: Mutex::new(State::Empty),
        }
    }
}

impl AtomicWaker {
    pub fn poll(&self, waker_ref: &Waker, waiting: impl FnOnce()) -> Poll<usize> {
        let mut state = self.state.lock();
        let (will_wake, was_empty) = match &*state {
            State::Empty => (false, true),
            State::Waiting(ref waker) => (waker.will_wake(waker_ref), false),
            State::Notified(ref token) => return Poll::Ready(token.clone()),
        };

        if !will_wake {
            if was_empty {
                waiting();
            }
            *state = State::Waiting(waker_ref.clone());
        }

        Poll::Pending
    }

    pub fn reset(&self, token: usize) -> bool {
        let mut state = self.state.lock();
        match &*state {
            State::Notified(ref t) if token.eq(t) => {
                *state = State::Empty;
                true
            }
            _ => false,
        }
    }

    pub fn wake(&self) -> Option<Waker> {
        let mut state = self.state.lock();
        match mem::replace(&mut *state, State::Notified(0)) {
            State::Empty => None,
            State::Waiting(waker) => Some(waker),
            State::Notified(token) => {
                *state = State::Notified(token.wrapping_add(1));
                None
            }
        }
    }
}
