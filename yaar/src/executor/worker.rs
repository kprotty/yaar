use super::{Node, Platform};
use core::{
    ptr::NonNull,
};

pub struct Worker<P: Platform> {
    node: NonNull<Node<P>>,
}