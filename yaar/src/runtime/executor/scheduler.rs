//! Numa-Aware, Multithreaded Scheduler based on
//! https://docs.google.com/document/u/0/d/1d3iI2QWURgDIsSR6G2275vMeQ_X7w-qxM2Vp7iGwwuM/pub.

use crate::{
    runtime::{
        platform::Platform,
        task::{GlobalQueue, LocalQueue, Task},
        Executor, with_executor_as,
    },
    util::CachePadded,
};
use core::{num::NonZeroUsize, ptr::NonNull, slice::from_raw_parts};
use yaar_lock::sync::CoreMutex;

pub enum RunError {
    /// No nodes were provided for the executor to run the future.
    EmptyNodes,
    /// The starting node index was not in range for the provide nodes.
    InvalidStart,
}

pub struct NodeExecutor<P: Platform> {
    platform: NonNull<P>,
    nodes_ptr: NonNull<NonNull<Node<P>>>,
    nodes_len: usize,
}

impl<P: Platform> NodeExecutor<P> {
    /// TODO: Run the task with an executor optimized for single threaded
    /// access.
    pub fn run_serial(_platform: &P, _task: &Task) {
        unimplemented!();
    }

    /// TODO: Run the task with a scheme akin to well-known executors such as
    /// tokio and async-std with a unified SMP thread-pool over multiple Nodes.
    pub fn run_smp(
        _platform: &P,
        _workers: &[Worker<P>],
        _max_threads: NonZeroUsize,
        _task: &Task,
    ) {
        unimplemented!();
    }

    pub fn run_using(
        platform: &P,
        start_node: usize,
        nodes: &[NonNull<Node<P>>],
        task: &Task,
    ) -> Result<(), RunError> {
        if nodes.len() == 0 {
            return Err(RunError::EmptyNodes);
        } else if start_node >= nodes.len() {
            return Err(RunError::InvalidStart);
        }

        with_executor_as(
            &Self {
                platform: NonNull::new(platform as *const _ as *mut _).unwrap(),
                nodes_ptr: NonNull::new(nodes.as_ptr() as *mut _).unwrap(),
                nodes_len: nodes.len(),
            },
            |executor| {
                let main_node = executor.nodes()[start_node];
                unimplemented!()
            },
        )
    }

    /// Get the array of Nodes passed into the run function.
    pub fn nodes(&self) -> &[&Node<P>] {
        unsafe { from_raw_parts(self.nodes_ptr.as_ptr() as *const _, self.nodes_len) }
    }

    /// Get the reference to the platform passed into the run function.
    pub fn platform(&self) -> &P {
        unsafe { &*self.platform.as_ptr() }
    }
}

unsafe impl<P: Platform> Sync for NodeExecutor<P> {}

impl<P: Platform> Executor for NodeExecutor<P> {
    fn schedule(&self, _task: &Task) {
        // TODO
    }
}

pub struct Node<P: Platform> {
    executor: NonNull<NodeExecutor<P>>,
}

pub struct Worker<P: Platform> {
    node: NonNull<Node<P>>,
}
