use super::{builder::Builder, queue::Queue};
use std::{
    pin::Pin, sync::atomic::{AtomicUsize, Ordering},
};

#[derive(Copy, Clone)]
struct Idle {
    index: Option<usize>,
    aba: usize,
}

impl Idle {
    const BITS: u32 = usize::BITS / 2;
    const MASK: usize = (1 << BITS) - 1;
}

impl Into<usize> for Idle {
    fn into(self) -> usize {
        let index = self.index.map(|i| i + 1).unwrap_or(0);
        assert!(index <= Self::MASK);

        let aba = self.aba & Self::MASK;
        (aba << Self::BITS) | index
    }
}

impl From<usize> for Idle {
    fn from(value: usize) -> Self {
        Self {
            index: match value & Self::MASK {
                0 => None,
                i => Some(i - 1),
            },
            aba: value >> Self::BITS,
        }
    }
}

pub struct Worker {
    idle_next: AtomicUsize,
    run_queue: Queue,
}

pub struct Executor {
    idle: AtomicUsize,
    pending: AtomicUsize,
    searching: AtomicUsize,
    injecting: AtomicUsize,
    workers: Pin<Box<[Worker]>>,
}

impl Executor {
    pub(super) fn mark_task_begin(&self) {

    }

    pub(super) fn mark_task_end(&self) {
        
    }

    pub(super) fn mark_thread_searching(&self) -> bool {

    }

    pub(super) fn mark_thread_discovered(&self) {

    }

    pub(super) fn mark_thread_sleeping(&self, index: usize, was_searching: bool) {

    } 

    pub(super) fn mark_thread_active(&self, was_searching: bool) -> Option<usize> {

    }
}