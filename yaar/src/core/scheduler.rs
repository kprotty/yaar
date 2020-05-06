use super::Node;
use core::{
    ptr::NonNull,
    num::NonZeroUsize,
};

pub struct Scheduler {
    nodes: NonNull<NonNull<Node>>,
    num_nodes: NonZeroUsize,
}