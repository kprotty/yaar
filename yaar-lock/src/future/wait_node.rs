use core::{
    cell::Cell,
    marker::PhantomPinned,
};

pub struct WaitNode {
    _pin: PhantomPinned,
}