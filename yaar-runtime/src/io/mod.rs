#[cfg(all(feature = "os", windows))]
mod windows;
#[cfg(all(feature = "os", windows))]
pub use windows::OsReactor;

#[cfg(all(feature = "os", target_os = "linux"))]
mod linux;
#[cfg(all(feature = "os", target_os = "linux"))]
pub use linux::OsReactor;

#[cfg(all(feature = "os", unix, not(target_os = "linux")))]
mod posix;
#[cfg(all(feature = "os", unix, not(target_os = "linux")))]
pub use posix::OsReactor;

use core::{cell::UnsafeCell, marker::Sync, mem, time::Duration};

pub trait Reactor {
    fn poll(&self, timeout: Option<Duration>) -> usize;
}

pub struct ReactorRef {
    ptr: *const (),
    _poll: unsafe fn(*const (), timeout: Option<Duration>) -> usize,
}

impl ReactorRef {
    #[inline]
    pub fn poll(&self, timeout: Option<Duration>) -> usize {
        unsafe { (self._poll)(self.ptr, timeout) }
    }
}

struct ReactorCell(UnsafeCell<Option<ReactorRef>>);

unsafe impl Sync for ReactorCell {}

static REACTOR_CELL: ReactorCell = ReactorCell(UnsafeCell::new(None));

pub fn with_reactor_as<R: Reactor, T>(reactor: &R, scoped: impl FnOnce(&R) -> T) -> T {
    let old_ref = mem::replace(
        unsafe { &mut *REACTOR_CELL.0.get() },
        Some(ReactorRef {
            ptr: reactor as *const _ as *const (),
            _poll: |ptr, timeout| unsafe { (&*(ptr as *const R)).poll(timeout) },
        }),
    );

    let result = scoped(reactor);
    unsafe { *(&mut *REACTOR_CELL.0.get()) = old_ref };
    result
}

pub fn with_reactor<T>(scoped: impl FnOnce(&ReactorRef) -> T) -> T {
    scoped(unsafe {
        (&*REACTOR_CELL.0.get())
            .as_ref()
            .expect("Reactor is not set. Make sure this is invoked only inside the scope of `with_reactor_as()`")
    })
}
