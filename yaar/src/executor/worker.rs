use super::{Node, Platform, Task};
use core::{
    ptr::NonNull,
};

pub struct Worker<P: Platform> {
    node: NonNull<Node<P>>,
    data: P::WorkerData,
}