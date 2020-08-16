use core::marker::PhantomPinned;

pub struct ThreadHandle {
    value: usize,
}

impl From<ThreadHandle> for usize {
    fn from(handle: ThreadHandle) -> usize {
        handle.value
    }
}

impl ThreadHandle {
    pub fn new(value: usize) -> Option<Self> {
        if value % 4 == 0 {
            Some(Self { value })
        } else {
            None
        }
    }

    pub unsafe fn new_unchecked(value: usize) -> Self {
        Self { value }
    }
}

pub struct Thread {
    _pinned: PhantomPinned,
}
