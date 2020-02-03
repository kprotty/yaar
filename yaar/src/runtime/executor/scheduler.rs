//! Numa-Aware, Multithreaded Scheduler based on
//! https://docs.google.com/document/u/0/d/1d3iI2QWURgDIsSR6G2275vMeQ_X7w-qxM2Vp7iGwwuM/pub.

use crate::{
    util::CachePadded,
    runtime::{
        platform::Platform,
    },
};
use core::{
    ptr::NonNull,
    sync::atomic::{AtomicUsize},
};

pub struct NodeExecutor<P: Platform> {
    platform: NonNull<P>,
    pending_tasks: AtomicUsize,
}