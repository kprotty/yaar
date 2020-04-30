
use core::{
    sync::atomic::AtomicUsize,
    mem::MaybeUninit,
};

pub struct TaskJoinHandle<T> {
    state: AtomicUsize,
    result: MaybeUninit<T>,
}

impl<T> TaskJoinHandle<T> {
    pub(super) const fn new() -> Self {
        Self {
            state: AtomicUsize::default(),
            result: MaybeUninit::uninit(),
        }
    }
}