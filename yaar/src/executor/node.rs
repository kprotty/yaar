use super::{Platform, Scheduler};
use core::{
    ptr::NonNull,
};

pub struct Node<P: Platform> {
    scheduler: NonNull<Scheduler<P>>,
}