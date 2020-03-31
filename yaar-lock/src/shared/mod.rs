use core::{
    ptr::null,
    pin::Pin,
    future::Future,
    task::{Poll, Waker, RawWaker, RawWakerVTable, Context},
};

mod mutex;
pub use mutex::*;

pub unsafe fn poll_sync<T>(mut future: impl Future<Output = T>) -> T {
    const VTABLE: RawWakerVTable = RawWakerVTable::new(
        |_| RawWaker::new(null(), &VTABLE),
        |_| unreachable!("poll_sync waker should not be woken"),
        |_| unreachable!("poll_sync waker should not be woken"),
        |_| {},
    );

    let waker = Waker::from_raw(RawWaker::new(null(), &VTABLE));
    let mut context = Context::from_waker(&waker);
    let future = Pin::new_unchecked(&mut future);
    
    match future.poll(&mut context) {
        Poll::Pending => unreachable!("poll_sync() should not return Poll::Pending"),
        Poll::Ready(value) => value,
    }
}