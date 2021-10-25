use std::{
    marker::{PhantomData, PhantomPinned},
    ptr::{self, NonNull},
    sync::atomic::{AtomicPtr, AtomicUsize, Ordering},
};

#[derive(Default)]
pub struct Task {
    pub(super) next: AtomicPtr<Task>,
    pub(super) vtable: Option<&'static TaskVTable>,
    _pinned: PhantomPinned,
}
