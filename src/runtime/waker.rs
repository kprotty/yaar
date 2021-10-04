use std::{
    cell::UnsafeCell,
    mem,
    sync::atomic::{AtomicU8, Ordering},
    task::{Poll, Waker},
};

#[derive(Copy, Clone, Debug, Eq, PartialEq)]
enum WakerState {
    Empty = 0,
    Updating = 1,
    Ready = 2,
    Notified = 3,
}

impl Into<u8> for WakerState {
    fn into(self) -> u8 {
        self as u8
    }
}

impl From<u8> for WakerState {
    fn from(value: u8) -> Self {
        [Self::Empty, Self::Updating, Self::Ready, Self::Notified][value]
    }
}

#[derive(Default)]
pub(crate) struct AtomicWaker {
    state: AtomicU8,
    waker: UnsafeCell<Option<Waker>>,
}

unsafe impl Send for AtomicWaker {}
unsafe impl Sync for AtomicWaker {}

impl AtomicWaker {
    /// Poll for a notification by `wake` on the AtomicWaker.
    /// If no notification is available, it updates the internal waker with the given `&Waker` and returns `Pending`.
    /// If the AtomicWaker was notified, it drops any pending waker updates and returns `Poll::Ready(())`.
    ///
    /// # Safety
    ///
    /// Must be called by the single consumer thread.
    /// Failure to do so could result in a data-race from reading/writing the `waker` field.
    pub unsafe fn poll(&self, waker: &Waker) -> Poll<()> {
        let state: WakerState = self.state.load(Ordering::Relaxed).into();
        match state {
            WakerState::Empty | WakerState::Ready => {}
            WakerState::Notified => return Poll::Ready(()),
            WakerState::Updating => unreachable!("AtomicWaker being polled concurrently"),
        }

        if let Err(state) = self.state.compare_exchange(
            state.into(),
            WakerState::Updating.into(),
            Ordering::Acquire,
            Ordering::Relaxed,
        ) {
            assert_eq!(WakerState::from(state), WakerState::Notified);
            return Poll::Ready(());
        }

        let will_wake = (&*self.waker.get())
            .as_ref()
            .map(|current_waker| waker.will_wake(current_waker))
            .unwrap_or(false);

        if !will_wake {
            match mem::replace(&mut *self.waker.get(), Some(waker.clone())) {
                Some(_dropped_waker) => assert_eq!(state, WakerState::Ready),
                None => assert_eq!(state, WakerState::Empty),
            }
        }

        if let Err(state) = self.state.compare_exchange(
            WakerState::Updating.into(),
            WakerState::Ready.into(),
            Ordering::Release,
            Ordering::Relaxed,
        ) {
            assert_eq!(WakerState::from(state), WakerState::Notified);
            *self.waker.get() = None;
            return Poll::Ready(());
        }

        Poll::Pending
    }

    /// Removes and drops any existing Waker registered in the AtomicWaker by `poll()`.
    /// This decreases the chance of a Waker being referenced for longer than necessary.
    ///
    /// # Safety
    ///
    /// Must be called by the single consumer thread (the same of which poll()s).
    /// Failure to do so could result in a data-race from reading/writing the `waker` field.
    pub unsafe fn detach(&self) {
        let state: WakerState = self.state.load(Ordering::Relaxed).into();
        match state {
            WakerState::Ready => {}
            WakerState::Empty | WakerState::Notified => return,
            WakerState::Updating => unreachable!("AtomicWaker detaching while polling"),
        }

        if let Err(state) = self.state.compare_exchange(
            WakerState::Ready.into(),
            WakerState::Empty.into(),
            Ordering::Acquire,
            Ordering::Relaxed,
        ) {
            return assert_eq!(
                WakerState::from(state),
                WakerState::Notified,
                "AtomicWaker detached with invalid state",
            );
        }

        *self.waker.get() = None;
    }

    pub fn wake(&self) {
        let state: WakerState = self
            .state
            .swap(WakerState::Notified.into(), Ordering::AcqRel)
            .into();

        match state {
            WakerState::Ready => {}
            WakerState::Empty | WakerState::Updating => return,
            WakerState::Notified => unreachable!("AtomicWaker::wake() called multiple times"),
        }

        mem::replace(&mut *self.waker.get(), None)
            .expect("AtomicWaker::wake() didn't find a Waker")
            .wake();
    }
}
