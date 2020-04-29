use super::{Platform, Scheduler, Worker};
use core::{
    ptr::NonNull,
    num::NonZeroUsize,
    sync::atomic::{AtomicUsize},
};

pub struct Node<P: Platform> {
    _pinned: PhantomPinned,
    scheduler: NonNull<Scheduler<P>>,
    workers_ptr: NonNull<NonNull<Worker<P>>>,
    workers_len: NonZeroUsize,
    idle_workers: AtomicUsize,
    suspended_workers: AtomicUsize,
}