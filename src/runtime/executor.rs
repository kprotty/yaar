use super::{builder::Builder, queue::Queue};
use std::{
    pin::Pin,
    sync::atomic::{AtomicUsize, Ordering},
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
    pub(super) run_queue: Queue,
}

pub struct Executor {
    idle: AtomicUsize,
    pending: AtomicUsize,
    searching: AtomicUsize,
    pub(super) workers: Pin<Box<[Worker]>>,
}

impl Executor {
    pub(super) fn mark_task_begin(&self) {
        let pending = self.pending.fetch_add(1, Ordering::Relaxed);
        assert_ne!(pending, usize::MAX);
    }

    pub fn mark_task_end(&self) -> bool {
        let pending = self.pending.fetch_sub(1, Ordering::AcqRel);
        pending == 1
    }

    pub fn try_inc_searching(&self) -> bool {
        let searching = self.searching.load(Ordering::Relaxed);
        assert!(searching <= self.workers.len());

        (2 * searching) < self.workers.len() && {
            self.inc_searching();
            true
        }
    }

    pub fn inc_searching(&self) {
        let searching = self.searching.fetch_add(1, Ordering::Relaxed);
        assert!(searching < self.workers.len());
    }

    pub fn dec_searching(&self) -> bool {
        let searching = self.searching.fetch_sub(1, Ordering::AcqRel);
        assert!(searching <= self.workers.len());
        assert_ne!(searching, 0);
        searching == 1
    }

    pub fn push_idle_worker(&self, worker_index: usize) {
        let _ = self
            .idle
            .fetch_update(Ordering::Release, Ordering::Relaxed, |idle| {
                let mut idle = Idle::from(idle);
                if let Some(index) = idle.index {
                    assert!(index <= self.workers.len());
                }

                self.workers[worker_index]
                    .idle_next
                    .store(idle.into(), Ordering::Relaxed);
                idle.index = Some(worker_index);
                idle.aba += 1;
                Some(idle.into())
            });
    }

    pub fn peek_idle_worker(&self) -> Option<usize> {
        let idle = self.idle.load(Ordering::Relaxed).into();
        Idle::from(idle).index
    }

    pub fn pop_idle_worker(&self) -> Option<usize> {
        self.idle
            .fetch_update(Ordering::AcqRel, Ordering::Acquire, |idle| {
                let mut idle = Idle::from(idle);
                let worker_index = idle.index?;

                let next: Idle = self.workers[worker_index]
                    .idle
                    .next
                    .load(Ordering::Relaxed)
                    .into();
                idle.index = next.index;
                Some(idle.into())
            })
            .and_then(|idle| Idle::from(idle).index)
            .ok()
    }
}
