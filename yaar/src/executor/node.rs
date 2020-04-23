use super::{Worker, Scheduler, Platform};
use core::{
    ptr::NonNull,
};

pub struct Node<P: Platform> {
    scheduler: NonNull<Scheduler<P>>,
    data: P::NodeData,
}