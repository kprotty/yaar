#![no_std]

pub trait Reactor {
    type Instant: Default + PartialOrd<Self::Instant>;

    fn poll(&self, timeout: Option<Self::Instant>, f: impl FnMut(usize));
}
