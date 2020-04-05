use crate::AutoResetEvent;
use core::{
    pin::Pin,
    cell::Cell,
    future::Future,
    sync::atomic::{AtomicUsize, Ordering},
    task::{Poll, Waker, Context},
};

const UNLOCKED: usize = 0;
const LOCKED: usize = 1;
const WAKING: usize = 2;
const MASK: usize = !(UNLOCKED | LOCKED | WAKING);

pub struct Mutex<E> {
    state: AtomicUsize,
}

