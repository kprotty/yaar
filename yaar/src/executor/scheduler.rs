use super::Platform;
use core::{
    ptr::NonNull,
};

pub struct Scheduler<P: Platform> {
    platform: NonNull<P>,
}