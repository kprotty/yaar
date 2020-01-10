
#[cfg(feature = "time")]
use crate::time::Clock;

#[cfg(feature = "io")]
use crate::reactor::Reactor;

#[cfg(feature = "rt-alloc")]
use core::alloc::GlobalAlloc;

use core::{
    pin::Pin,
    ptr::null,
    task::{Poll, Context, Waker, RawWaker, RawWakerVTable},
    future::Future,
};

pub fn run<T>(
    #[cfg(feature = "rt-alloc")] allocator: impl GlobalAlloc,
    #[cfg(feature = "io")] reactor: impl Reactor,
    #[cfg(feature = "time")] clock: impl Clock,
    mut future: impl Future<Output = T>,
) -> T {
    
    let waker = unsafe {
        const VTABLE: RawWakerVTable = RawWakerVTable::new(
            |ptr| RawWaker::new(ptr, &VTABLE),
            |_| panic!("wake() not implemented"),
            |_| panic!("wake_by_ref() not implemented"),
            |_| {},
        );
        Waker::from_raw(RawWaker::new(null(), &VTABLE))
    };
    
    let mut context = Context::from_waker(&waker);
    loop {
        let fut = unsafe { Pin::new_unchecked(&mut future) };
        match fut.poll(&mut context) {
            Poll::Pending => {},
            Poll::Ready(value) => return value,
        }
    }
}