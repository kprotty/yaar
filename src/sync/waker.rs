use std::{
    cell::UnsafeCell,
    mem,
    sync::atomic::{AtomicUsize, Ordering},
    task::{Poll, Waker},
};

#[derive(Copy, Clone, Eq, PartialEq, Debug)]
enum Status {
    Empty = 0,
    Updating = 1,
    Ready = 2,
    Waking = 3,
    Notified = 4,
}

#[derive(Copy, Clone)]
struct State {
    token: usize,
    status: Status,
}

impl Into<usize> for State {
    fn into(self) -> usize {
        (self.token << 3) | (self.status as usize)
    }
}

impl From<usize> for State {
    fn from(value: usize) -> Self {
        Self {
            token: value >> 3,
            status: match value & 0b111 {
                0 => Status::Empty,
                1 => Status::Updating,
                2 => Status::Ready,
                3 => Status::Waking,
                4 => Status::Notified,
                _ => unreachable!(),
            },
        }
    }
}

#[derive(Default)]
pub struct AtomicWaker {
    state: AtomicUsize,
    waker: UnsafeCell<Option<Waker>>,
}

impl AtomicWaker {
    pub const fn new() -> Self {
        Self {
            state: AtomicUsize::new(0),
            waker: UnsafeCell::new(None),
        }
    }

    pub fn poll(&self, waker_ref: &Waker, waiting: impl FnOnce()) -> Poll<usize> {
        let state: State = self.state.load(Ordering::Acquire).into();
        match state.status {
            Status::Updating => unreachable!("multiple threads polling AtomicWaker"),
            Status::Ready
            Status::Updating | Status::Waking => {
                waker_ref.wake_by_ref();
                return Poll::Pending,
            Status::Notified => return Poll::Ready(state.token),
            
        }
    }

    pub fn reset(&self, token: usize) -> bool {
        let expected = State {
            token,
            status: Status::Notified,
        };

        self.state
            .fetch_update(Ordering::AcqRel, Ordering::Acquire, |state| {
                let mut state = State::from(state);
                if state == expected {
                    state.status = Status::Empty;
                    Some(state.into())
                } else {
                    None
                }
            })
            .is_ok()
    }

    pub fn wake(&self) -> Option<Waker> {
        let mut state: State = self.state.load(Ordering::Relaxed).into();
        loop {
            let new_state = match state.status {
                Status::Empty => State {
                    token: state.token,
                    status: Status::Notified,
                },
                Status::Updating => State {
                    token: state.token + 1,
                    status: Status::Updating,
                },
                Status::Ready => {
                    if let Err(e) = self.state.compare_exchange_weak(
                        state.into(),
                        (State {
                            token: state.token,
                            status: Status::Waking,
                        })
                        .into(),
                        Ordering::Acquire,
                        Ordering::Relaxed,
                    ) {
                        state = e.into();
                        continue;
                    }

                    let waker = mem::replace(unsafe { &mut *self.waker.get() }, None);
                    let waker = waker.expect("wake() saw Ready without a Waker");

                    state.status = Status::Notified;
                    self.state.store(state.into(), Ordering::Release);
                    return Some(waker);
                }
                Status::Waking => return None,
                Status::Notified => State {
                    token: state.token + 1,
                    status: Status::Notified,
                },
            };

            match self.state.compare_exchange_weak(
                state.into(),
                new_state.into(),
                Ordering::Release,
                Ordering::Relaxed,
            ) {
                Ok(_) => return None,
                Err(e) => state = e.into(),
            }
        }
    }
}
