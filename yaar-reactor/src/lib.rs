#![no_std]

use core::{cell::UnsafeCell, marker::Sync, mem, num::NonZeroUsize, time::Duration};

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
    fn poll(&self, timeout: Option<Duration>) -> Result<NonZeroUsize, PollError>;
}

/// A virtual reference to the current global scoped Reactor.
#[derive(Debug, PartialEq)]
pub struct ReactorRef {
    ptr: *const (),
    _poll: unsafe fn(*const (), timeout: Option<Duration>) -> Result<NonZeroUsize, PollError>,
}

impl ReactorRef {
    /// Proxy for [`Reactor::poll`].
    #[inline]
    pub fn poll(&self, timeout: Option<Duration>) -> Result<NonZeroUsize, PollError> {
        unsafe { (self._poll)(self.ptr, timeout) }
    }
}

struct ReactorCell(UnsafeCell<Option<&'static ReactorRef>>);

unsafe impl Sync for ReactorCell {}

static REACTOR_CELL: ReactorCell = ReactorCell(UnsafeCell::new(None));

/// Set the global reactor instance for a given function scope.
/// The global reactor supports being modified recursively as it
/// is restored via stack but should not be called in parellel.
pub fn with_reactor_as<R: Reactor, T>(reactor: &R, scoped: impl FnOnce(&R) -> T) -> T {
    // The vtable for the provided reactor.
    // Wish it could be `const` but rust doesnt currently
    // support function type parameters in const :(
    let reactor_ref = ReactorRef {
        ptr: reactor as *const _ as *const (),
        _poll: |ptr, timeout| unsafe { (&*(ptr as *const R)).poll(timeout) },
    };

    // promote the reactor_ref to &'static ReactorRef for storage in the global reactor cell.
    let static_ref = unsafe { Some(&*(&reactor_ref as *const _)) };
    let old_ref = unsafe { mem::replace(&mut *REACTOR_CELL.0.get(), static_ref) };
    let result = scoped(reactor);
    let our_ref = unsafe { mem::replace(&mut *REACTOR_CELL.0.get(), old_ref) };

    // ensure that the stack of reactor_ref's invariant is maintained.
    debug_assert_eq!(our_ref, static_ref);
    result
}

/// Used to acquire a reference the global reactor.
pub fn with_reactor<T>(scoped: impl FnOnce(&ReactorRef) -> T) -> T {
    scoped(unsafe {
        (&*REACTOR_CELL.0.get())
            .as_ref()
            .expect("Reactor is not set. Make sure this is invoked only inside the scope of `with_reactor_as()`")
    })
}
