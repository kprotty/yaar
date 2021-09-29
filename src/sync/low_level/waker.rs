use std::{
    cell::UnsafeCell,
    mem,
    sync::atomic::{AtomicU8, Ordering},
    task::Waker,
};

#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum WakerUpdate {
    Empty,
    Replaced,
    Interrupted,
    Notified,
}

#[derive(Copy, Clone, Debug, Eq, PartialEq)]
enum WakerState {
    Empty = 0,
    Updating = 1,
    Ready = 2,
    Waking = 3,
}

impl From<u8> for WakerState {
    fn from(value: u8) -> Self {
        match value {
            0 => Self::Empty,
            1 => Self::Updating,
            2 => Self::Ready,
            3 => Self::Waking,
            _ => unreachable!("invalid WakerState"),
        }
    }
}

#[derive(Default)]
pub struct AtomicWaker {
    state: AtomicU8,
    waker: UnsafeCell<Option<Waker>>,
}

unsafe impl Send for AtomicWaker {}
unsafe impl Sync for AtomicWaker {}

impl AtomicWaker {
    pub const fn new() -> Self {
        Self {
            state: AtomicU8::new(WakerState::Empty as u8),
            waker: UnsafeCell::new(None),
        }
    }

    pub fn wake(&self) -> Option<Waker> {
        let state: WakerState = self
            .state
            .swap(WakerState::Waking as u8, Ordering::AcqRel)
            .into();

        if state == WakerState::Ready {
            return Some(mem::replace(unsafe { &mut *self.waker.get() }, None)
            .expect("waker state was Ready without a Waker"));
        }

        None
    }

    pub fn is_notified(&self) -> bool {
        let state: WakerState = self.state.load(Ordering::Acquire).into();
        state == WakerState::Waking
    }

    pub fn detach(&self) -> Option<Waker> {
        let state: WakerState = self.state.load(Ordering::Acquire).into();
        match state {
            WakerState::Empty => return None,
            WakerState::Ready => {}
            WakerState::Updating => unreachable!("multiple threads trying to update Waker"),
            WakerState::Waking => return None,
        }

        if let Err(new_state) = self.state.compare_exchange(
            WakerState::Ready as u8,
            WakerState::Updating as u8,
            Ordering::Acquire,
            Ordering::Acquire,
        ) {
            let new_state: WakerState = new_state.into();
            assert_eq!(new_state, WakerState::Waking);
            return None;
        }

        let waker = mem::replace(unsafe { &mut *self.waker.get() }, None)
            .expect("detach consumed an invalid waker");

        if let Err(new_state) = self.state.compare_exchange(
            WakerState::Updating as u8,
            WakerState::Empty as u8,
            Ordering::AcqRel,
            Ordering::Acquire,
        ) {
            let new_state: WakerState = new_state.into();
            assert_eq!(new_state, WakerState::Waking);
        }

        Some(waker)
    }

    pub unsafe fn register(&self, waker_ref: &Waker) -> WakerUpdate {
        let state: WakerState = self.state.load(Ordering::Acquire).into();
        match state {
            WakerState::Empty | WakerState::Ready => {}
            WakerState::Updating => unreachable!("multiple threads trying to update Waker"),
            WakerState::Waking => return WakerUpdate::Notified,
        }

        if let Err(new_state) = self.state.compare_exchange(
            state as u8,
            WakerState::Updating as u8,
            Ordering::Acquire,
            Ordering::Acquire,
        ) {
            let new_state: WakerState = new_state.into();
            assert_eq!(new_state, WakerState::Waking);
            return WakerUpdate::Notified;
        }

        let will_wake = (&*self.waker.get())
            .as_ref()
            .map(|waker| waker_ref.will_wake(waker))
            .unwrap_or(false);

        if !will_wake {
            match mem::replace(&mut *self.waker.get(), Some(waker_ref.clone())) {
                Some(_dropped_waker) => assert_eq!(state, WakerState::Ready),
                None => assert_eq!(state, WakerState::Empty),
            }
        }

        if let Err(new_state) = self.state.compare_exchange(
            WakerState::Updating as u8,
            WakerState::Ready as u8,
            Ordering::AcqRel,
            Ordering::Acquire,
        ) {
            let new_state: WakerState = new_state.into();
            assert_eq!(new_state, WakerState::Waking);
            *self.waker.get() = None;
            return WakerUpdate::Interrupted;
        }

        match state {
            WakerState::Ready => WakerUpdate::Replaced,
            WakerState::Empty => WakerUpdate::Empty,
            _ => unreachable!(),
        }
    }

    pub unsafe fn reset(&self) {
        let new_state = WakerState::Empty as u8;
        self.state.store(new_state, Ordering::Relaxed);
    }
}
