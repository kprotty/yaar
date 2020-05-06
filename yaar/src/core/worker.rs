use super::Node;
use core::{
    ptr::NonNull,
};

pub struct Worker {
    node: NonNull<Node>,
}