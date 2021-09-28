use super::super::sync::low_level::AutoResetEvent;
use std::{
    pin::Pin,
    sync::atomic::{AtomicUsize, Ordering},
};

#[derive(Default)]
pub struct IdleNode {
    next: AtomicUsize,
    event: AutoResetEvent,
}

pub trait IdleNodeProvider {
    fn with<T>(&self, index: usize, f: impl FnOnce(Pin<&IdleNode>) -> T) -> T;
}

#[derive(Copy, Clone, Debug, Eq, PartialEq)]
enum IdleIndex {
    Empty,
    Waiting(usize),
    Notified,
    Shutdown,
}

#[derive(Copy, Clone)]
struct IdleState {
    aba: usize,
    index: IdleIndex,
}

impl IdleState {
    const BITS: u32 = usize::BITS / 2;
    const MASK: usize = (1 << Self::BITS) - 1;

    const SHUTDOWN: usize = Self::MASK;
    const NOTIFIED: usize = Self::MASK - 1;
    const MAX_INDEX: usize = (Self::MASK - 2) - 1;

    fn inc_aba(&mut self) {
        self.aba = (self.aba + 1) & Self::MASK;
    }
}

impl From<usize> for IdleState {
    fn from(value: usize) -> Self {
        Self {
            aba: value >> Self::BITS,
            index: match value & Self::MASK {
                Self::SHUTDOWN => IdleIndex::Shutdown,
                Self::NOTIFIED => IdleIndex::Notified,
                0 => IdleIndex::Empty,
                index => IdleIndex::Waiting(index - 1),
            },
        }
    }
}

impl Into<usize> for IdleState {
    fn into(self) -> usize {
        assert!(self.aba <= Self::MASK);
        let aba = self.aba << Self::BITS;

        aba | match self.index {
            IdleIndex::Shutdown => Self::SHUTDOWN,
            IdleIndex::Notified => Self::NOTIFIED,
            IdleIndex::Empty => 0,
            IdleIndex::Waiting(index) => {
                assert!(index <= Self::MAX_INDEX);
                index + 1
            }
        }
    }
}

#[derive(Default)]
pub struct IdleQueue {
    state: AtomicUsize,
}

impl IdleQueue {
    pub fn wait<P: IdleNodeProvider>(
        &self,
        node_provider: P,
        index: usize,
        validate: impl Fn() -> bool,
    ) {
        self.state
            .fetch_update(Ordering::AcqRel, Ordering::Acquire, |state| {
                let mut state: IdleState = state.into();
                if !validate() {
                    return None;
                }

                state.index = match state.index {
                    IdleIndex::Shutdown => return None,
                    IdleIndex::Notified => IdleIndex::Empty,
                    IdleIndex::Empty | IdleIndex::Waiting(_) => {
                        node_provider.with(index, |node| {
                            node.next.store(state.into(), Ordering::Relaxed)
                        });
                        state.inc_aba();
                        IdleIndex::Waiting(index)
                    }
                };

                Some(state.into())
            })
            .map(IdleState::from)
            .map(|state| match state.index {
                IdleIndex::Shutdown => unreachable!(),
                IdleIndex::Notified => {}
                _ => node_provider.with(index, |node| unsafe {
                    Pin::map_unchecked(node, |node| &node.event).wait()
                }),
            })
            .unwrap_or(())
    }

    pub fn signal<P: IdleNodeProvider>(&self, node_provider: P) -> bool {
        self.state
            .fetch_update(Ordering::AcqRel, Ordering::Acquire, |state| {
                let mut state: IdleState = state.into();

                state.index = match state.index {
                    IdleIndex::Shutdown => return None,
                    IdleIndex::Notified => return None,
                    IdleIndex::Empty => IdleIndex::Notified,
                    IdleIndex::Waiting(index) => node_provider.with(index, |node| {
                        let next = node.next.load(Ordering::Relaxed);
                        IdleState::from(next).index
                    }),
                };

                Some(state.into())
            })
            .ok()
            .map(IdleState::from)
            .and_then(|state| match state.index {
                IdleIndex::Waiting(index) => Some(index),
                _ => None,
            })
            .map(|index| {
                node_provider.with(index, |node| unsafe {
                    Pin::map_unchecked(node, |node| &node.event).notify()
                })
            })
            .is_some()
    }

    pub fn shutdown<P: IdleNodeProvider>(&self, node_provider: P) {
        let new_state = IdleState {
            aba: 0,
            index: IdleIndex::Shutdown,
        };

        let mut state: IdleState = self.state.swap(new_state.into(), Ordering::AcqRel).into();

        while let IdleIndex::Waiting(index) = state.index {
            node_provider.with(index, |node| unsafe {
                state = node.next.load(Ordering::Relaxed).into();
                Pin::map_unchecked(node, |node| &node.event).notify();
            })
        }
    }
}
