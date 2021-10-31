use parking_lot::Mutex;
use std::{
    mem,
    sync::atomic::{AtomicU8, Ordering},
    task::{Poll, Waker},
};

#[derive(Copy, Clone, Eq, PartialEq, Debug)]
enum Status {
    Empty = 0,
    Updating = 1,
    Ready = 2,
    Notified = 3,
}

#[derive(Copy, Clone)]
struct State {
    token: u8,
    status: Status,
}

impl Into<u8> for State {
    fn into(self) -> u8 {
        (self.token << 2) | (self.status as u8)
    }
}

impl From<u8> for State {
    fn from(value: u8) -> Self {
        Self {
            token: value >> 2,
            status: match value & 0b11 {
                0 => Status::Empty,
                1 => Status::Updating,
                2 => Status::Ready,
                3 => Status::Notified,
                _ => unreachable!(),
            },
        }
    }
}

#[derive(Default)]
pub struct AtomicWaker {
    state: AtomicU8,
    waker: Mutex<Option<Waker>>,
}

impl AtomicWaker {
    pub const fn new() -> Self {
        Self {
            state: AtomicU8::new(0),
            waker: parking_lot::const_mutex(None),
        }
    }

    pub fn poll(
        &self,
        waker_ref: &Waker,
        waiting: impl FnOnce(),
        cancelled: impl FnOnce(),
    ) -> Poll<u8> {
        let mut state: State = self.state.load(Ordering::Acquire).into();
        match state.status {
            Status::Empty | Status::Ready => {}
            Status::Notified => return Poll::Ready(state.token),
            Status::Updating => unreachable!("multiple threads polling same AtomicWaker"),
        }

        if let Err(e) = self.state.compare_exchange(
            state.into(),
            (State {
                token: state.token,
                status: Status::Updating,
            })
            .into(),
            Ordering::Acquire,
            Ordering::Acquire,
        ) {
            state = e.into();
            assert_eq!(state.status, Status::Notified);
            return Poll::Ready(state.token);
        }

        {
            let mut waker = self.waker.lock();
            let will_wake = waker
                .as_ref()
                .map(|w| w.will_wake(waker_ref))
                .unwrap_or(false);

            if !will_wake {
                *waker = Some(waker_ref.clone());
            }
        }

        if let Err(e) = self.state.compare_exchange(
            (State {
                token: state.token,
                status: Status::Updating,
            })
            .into(),
            (State {
                token: state.token,
                status: Status::Ready,
            })
            .into(),
            Ordering::AcqRel,
            Ordering::Acquire,
        ) {
            *self.waker.lock() = None;
            if state.status == Status::Ready {
                cancelled();
            }

            state = e.into();
            assert_eq!(state.status, Status::Notified);
            return Poll::Ready(state.token);
        }

        match state.status {
            Status::Empty => waiting(),
            Status::Ready => {}
            _ => unreachable!(),
        }

        Poll::Pending
    }

    pub fn reset(&self, token: u8) -> Result<(), u8> {
        match self.state.compare_exchange(
            (State {
                token,
                status: Status::Notified,
            })
            .into(),
            (State {
                token,
                status: Status::Empty,
            })
            .into(),
            Ordering::AcqRel,
            Ordering::Acquire,
        ) {
            Ok(_) => Ok(()),
            Err(state) => {
                let state = State::from(state);
                assert_eq!(state.status, Status::Notified);
                assert_ne!(state.token, token);
                Err(state.token)
            }
        }
    }

    pub fn detach(&self) -> Option<Waker> {
        self.state
            .fetch_update(Ordering::Acquire, Ordering::Relaxed, |state| {
                let mut state = State::from(state);
                state.status = match state.status {
                    Status::Ready => Status::Empty,
                    Status::Empty | Status::Notified => return None,
                    Status::Updating => unreachable!("detaching AtomicWaker with pending poll()"),
                };
                Some(state.into())
            })
            .ok()
            .and_then(|_| mem::replace(&mut *self.waker.lock(), None))
    }

    pub fn wake(&self) -> Option<Waker> {
        self.state
            .fetch_update(Ordering::AcqRel, Ordering::Relaxed, |state| {
                let mut state = State::from(state);
                state.token += (state.status == Status::Notified) as u8;
                state.status = Status::Notified;
                Some(state.into())
            })
            .ok()
            .and_then(|state| match State::from(state).status {
                Status::Ready => mem::replace(&mut *self.waker.lock(), None),
                _ => None,
            })
    }
}
