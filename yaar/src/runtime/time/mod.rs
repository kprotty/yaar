use core::{
    ptr::NonNull,
}

pub trait Clock: Sync {
    type Instant;
    type DelayQueue: DelayQueue<Self::Instant>;

    fn now(&self) -> Self::Instant;
}

pub trait DelayQueue<Instant>: From<Instant> {
    type Entry;

    fn schedule(
        &self,
        entry: NonNull<Self::Entry>,
    ) -> Result<(), NonNull<Self::Entry>>;

    fn poll_expired(
        &self,
        now: Instant,
    ) -> Result<NonNull<Entry>, Option<Instant>>;
}
