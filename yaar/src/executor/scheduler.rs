use core::ptr::NonNull;

pub struct Scheduler<P> {
    platform: NonNull<P>,
}
