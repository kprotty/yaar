use core::marker::PhantomPinned;

pub struct Pool {
    _pinned: PhantomPinned,
}
