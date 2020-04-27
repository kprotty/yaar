use super::{Platform, Node, Task};
use core::{
    ptr::NonNull,
    num::NonZeroUsize,
    slice::from_raw_parts,
};

pub struct Scheduler<P: Platform> {
    platform: NonNull<P>,
    nodes_ptr: NonNull<NonNull<Node<P>>>,
    nodes_len: NonZeroUsize,
}

impl<P: Platform> Scheduler<P> {
    pub unsafe fn run<P: Platform>(
        task: NonNull<Task>,
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

