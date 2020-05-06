use super::{Scheduler, Worker};
use core::{
    ptr::NonNull,
    num::NonZeroUsize,
};

pub enum NodeError {
    EmptyWorkers,
    TooManyWorkers
}

pub struct Node {
    pub(crate) scheduler: NonNull<Scheduler>,
    workers: NonNull<NonNull<Worker>>,
    num_workers: NonZeroUsize,
}

impl Node {
    const MAX_WORKERS: usize = !0usize.count_ones();

    pub fn new(workers: &[NonNull<Worker<P>>]) -> Result<Self, NodeError> {
        unsafe {
            Ok(Self {
                scheduler: NonNull::dangling(),
                workers: NonNull::new_unchecked(workers.as_ptr()),
                num_workers: match workers.len() {
                    1..MAX_WORKERS => NonZeroUsize::new_unchecked(workers.len()),
                    0 => return Err(NodeError::EmptyWorkers),
                    _ => return Err(NodeError::TooManyWorkers),
                },
                data
            })
        }
    }
}