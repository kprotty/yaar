use std::task::{Poll, Waker};

#[derive(Copy, Clone, Eq, PartialEq)]
struct FairState {
    tick: u8,
    be_fair: bool,
}

impl Into<u8> for FairState {
    fn into(self) -> u8 {
        (self.tick << 1) | (self.be_fair as u8)
    }
}

impl From<u8> for FairState {
    fn from(value: u8) -> Self {
        Self {
            tick: value >> 1,
            be_fair: value & 1 != 0,
        }
    }
}

#[derive(Default)]
pub struct PollFairness {
    state: u8,
}

impl PollFairness {
    pub fn poll_fair<T>(
        &mut self,
        waker_ref: &Waker,
        do_poll: impl FnOnce() -> Poll<T>,
    ) -> Poll<T> {
        let mut state: FairState = self.state.into();

        if state.be_fair {
            state.be_fair = false;
            self.state = state.into();

            waker_ref.wake_by_ref();
            return Poll::Pending;
        }

        let result = match do_poll() {
            Poll::Ready(result) => result,
            Poll::Pending => return Poll::Pending,
        };

        state.tick += 1;
        state.be_fair = state.tick == 0;
        self.state = state.into();

        Poll::Ready(result)
    }
}