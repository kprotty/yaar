use super::{Platform, Scheduler, Worker};
use core::{
    ptr::NonNull,
    num::NonZeroUsize,
};

pub enum NodeError {
    EmptyWorkers,
    TooManyWorkers
}

pub struct Node<P: Platform> {
    pub(crate) scheduler: NonNull<Scheduler<P>>,
    workers: NonNull<NonNull<Worker<P>>>,
    num_workers: NonZeroUsize,
    pub data: P::NodeData,
}

impl<P: Platform> Node<P> {
    const MAX_WORKERS: usize = !0.count_ones();

    pub fn new(
        data: P::NodeData,
        workers: &[NonNull<Worker<P>>],
    ) -> Result<Self, NodeError> {
        unsafe {
            Ok(Self {
                scheduler: NonNull::dangling(),
                workers: NonNull::new_unchecked(workers.as_ptr()),
                num_workers: match workers.len() {
                    1..MAX_WORKERS => NonZeroUsize::new(workers.len()),
                    0 => return Err(NodeError::EmptyWorkers),
                    _ => return Err(NodeError::TooManyWorkers),
                },
                data
            })
        }
    }
}