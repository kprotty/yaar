use super::ThreadParker;
use crate::shared::{WordEvent, WaitNode};
use core::{fmt, marker::PhantomData};

pub struct RawResetEvent<Parker> {
    event: WordEvent,
    phantom: PhantomData<Parker>,
}

unsafe impl<Parker: Send> Send for RawResetEvent<Parker> {}
unsafe impl<Parker: Sync> Sync for RawResetEvent<Parker> {}

impl<Parker> fmt::Debug for RawResetEvent<Parker> {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        f.debug_struct("ResetEvent").finish()
    }
}

impl<Parker> RawResetEvent<Parker> {
    /// Creates a new ResetEvent usingthe given state.
    pub const fn new() -> Self {
        Self {
            event: WordEvent::new(),
            phantom: PhantomData,
        }
    }

    /// Sets the event, waking up any threads waiting on the event.
    #[inline]
    pub fn reset(&self) {
        self.event.reset()
    }

    /// Returns whether the event is set.
    #[inline]
    pub fn is_set(&self) -> bool {
        self.event.is_set()
    }
}

/*
impl<Parker: ThreadParker> RawResetEvent<Parker> {
    pub fn set(&self) {

    }

    pub fn wait(&self)
}
*/