use super::{Platform, Node, Runnable};
use core::{
    pin::Pin,
    ptr::NonNull,
    num::NonZeroUsize,
};

pub struct Scheduler<P: Platform> {
    platform: NonNull<P>,
    nodes_ptr: NonNull<NonNull<Node<P>>>,
    nodes_len: NonZeroUsize,
}

unsafe impl<P: Platform> Sync for Scheduler<P> {}

impl<P: Platform> Scheduler<P> {
    pub unsafe fn run(
        runnable: Pin<&Runnable>,
        platform: &P,
        nodes: &[NonNull<Node<P>>],
    ) {
        let scheduler = Self {
            platform: NonNull::from(platform),
            nodes_ptr: NonNull::new_unchecked(nodes.as_ptr() as *mut _),
            nodes_len: NonZeroUsize::new_unchecked(nodes.len()),
        };


    }
}