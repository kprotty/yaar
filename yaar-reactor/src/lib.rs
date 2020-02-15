#![no_std]

pub trait Reactor {
    type Instant: Default;

    fn poll(&self, timeout: Option<Self::Instant>, f: impl FnMut(usize));
}
