use super::{Task, Node};
use core::ptr::NonNull;

pub struct Scheduler<P> {
    platform: NonNull<P>,
    nodes_ptr: NonNull<Node<P>>,
}

impl<P: Platform> Scheduler<P> {
    pub fn run(
        task: NonNull<Task>,
        platform: NonNull<P>,
        nodes: &[NonNull<Node<P>>],
    ) {

    }
}