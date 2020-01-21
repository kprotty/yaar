#![no_std]

use core::{cell::UnsafeCell, marker::Sync, mem, num::NonZeroUsize};

pub enum PollError {
    TimedOut,
}

/// Abstraction over a backend for non-blocking and async io.
pub trait Reactor: Sync {
    // ~ brainstorming
    // fn read(
    //    &self,
    //    waker: Waker,
    //    handle: usize,
    //    buffer: &[iovec::IoVec],
    //    options:Option<enum{&Address, UsizeFilePos}>
    // ) -> Result<NonZeroUsize, ReadError>

    /// Poll for non-blocking IO and call wake() or equivalent for any wakers which were notified.
    /// Returns either the amount of wakers woken up or an error indicating timeout.
    fn poll(&self, timeout_ms: Option<u64>) -> Result<NonZeroUsize, PollError>;
}