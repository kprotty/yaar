use super::shared_u64::SharedU64;
use crate::runtime::internal::context::Context;
use crate::sync::internal::waker::AtomicWaker;
use parking_lot::Mutex;
use std::{
    collections::btree_map::Entry as TreeEntry,
    collections::BTreeMap,
    mem::drop,
    num::NonZeroU64,
    sync::atomic::{AtomicBool, Ordering},
    sync::Arc,
    time::{Duration, Instant},
};

/// A shared refernce to a RawEntry that may be scheduled in the timer Queue
pub type Entry = Arc<RawEntry>;

#[derive(Default)]
pub struct RawEntry {
    pub waker: AtomicWaker,
    queued: AtomicBool,
}

/// A timestamp in milliseconds since the start epoch of a timer Queue
pub type Deadline = u64;

/// A scheduled registration of an Entry which will be notified at a Deadline
pub struct Delay {
    pub deadline: Deadline,
    pub entry: Entry,
}

// A small, quick cache of items
#[derive(Default)]
struct Cache<T> {
    cached: Vec<T>,
}

impl<T: Default> Cache<T> {
    const MAX_CACHED: usize = 64;

    fn alloc(&mut self) -> T {
        let index = self.cached.len().checked_sub(1);
        index
            .map(|i| self.cached.swap_remove(i))
            .unwrap_or_else(|| T::default())
    }

    fn free(&mut self, value: T) {
        if self.cached.len() < Self::MAX_CACHED {
            self.cached.push(value)
        } else {
            drop(value);
        }
    }
}

type EntryList = Vec<Entry>;

// A list of entries that were expired due to a timer Queue::poll()
#[derive(Default)]
pub struct Expired(Vec<Entry>);

impl Expired {
    pub fn pending(&self) -> bool {
        self.0.len() > 0
    }

    pub fn process(&mut self) {
        self.0.drain(..).for_each(|entry| entry.waker.wake());
    }
}

#[derive(Default)]
struct State {
    current: Deadline,
    entry_cache: Cache<Entry>,
    list_cache: Cache<EntryList>,
    tree: BTreeMap<Deadline, EntryList>,
}

pub struct Queue {
    expires: SharedU64,
    state: Mutex<State>,
    started: Instant,
}

impl Queue {
    pub fn new(instant: Instant) -> Self {
        Self {
            expires: SharedU64::default(),
            state: Mutex::new(State::default()),
            started: instant,
        }
    }

    /// Get a reference to the current DelayQueue when running inside a runtime context
    pub fn with<F>(f: impl FnOnce(&Arc<Self>) -> F) -> F {
        Context::with(|context| {
            // Use the local worker or a random one
            let worker_index = context.worker_index.get().unwrap_or_else(|| {
                let rng = context.rng.borrow_mut().gen();
                rng % context.executor.workers.len()
            });

            f(&context.executor.workers[worker_index].timer_queue)
        })
    }

    /// Convert an Instant into u64 millis using the Delay's Instant as the starting epoch
    pub fn since(&self, instant: Instant) -> Deadline {
        instant
            .checked_duration_since(self.started)
            .map(|duration| duration.as_millis().try_into().unwrap_or(Deadline::MAX))
            .unwrap_or(0)
    }

    /// Peek the next millis deadline that the DelayQueue will expire at
    pub fn expires(&self) -> Option<Deadline> {
        NonZeroU64::new(self.expires.load()).map(|e| e.get())
    }

    /// Update the DelayQueue to the `current` time and expire any timer Entries into `expired`.
    pub fn poll(&self, current: Deadline, expired: &mut Expired) {
        let mut state = self.state.lock();
        state.current = state.current.max(current);

        while let Some(deadline) = state.tree.iter().map(|i| *i.0).next() {
            if deadline > state.current {
                self.expires.store(deadline);
                return;
            }

            let mut list = state.tree.remove(&deadline).unwrap();
            let entries = list.drain(..).map(|entry| {
                assert!(entry.queued.load(Ordering::Relaxed));
                entry.queued.store(false, Ordering::Relaxed);
                entry
            });

            expired.0.extend(entries);
            state.list_cache.free(list);
        }

        self.expires.store(0);
    }

    /// Schedule an Entry to expire at the given `delay`.
    /// Returns `None` if the delay has already expired.
    ///
    /// NOTE: allocates a resource that must be freed with `complete()`.
    #[cold]
    pub fn schedule(&self, delay: Result<Duration, Instant>) -> Option<Delay> {
        if matches!(delay, Ok(Duration::ZERO)) {
            return None;
        }

        let mut state = self.state.lock();
        state.current = self.since(Instant::now());

        let deadline = delay
            .map(|duration| {
                duration
                    .as_millis()
                    .try_into()
                    .ok()
                    .map(|millis: Deadline| millis.max(1))
                    .and_then(|m| m.checked_add(state.current))
                    .unwrap_or(Deadline::MAX)
            })
            .unwrap_or_else(|instant| self.since(instant));

        if deadline <= state.current {
            return None;
        }

        // expires = expires.map(|e| e.min(deadline)).unwrap_or(deadline)
        match NonZeroU64::new(self.expires.get()) {
            Some(e) if deadline < e.get() => self.expires.store(deadline),
            Some(_) => {}
            None => self.expires.store(deadline),
        }

        let entry = state.entry_cache.alloc();
        let mut list = Some(state.list_cache.alloc());

        match state.tree.entry(deadline) {
            TreeEntry::Occupied(ref mut occupied) => occupied.get_mut().push(entry.clone()),
            TreeEntry::Vacant(vacant) => {
                let list = list.take().unwrap();
                vacant.insert(list).push(entry.clone());
            }
        }

        // A new Queue wasn't needed so free it.
        // We can't allocate a Queue when we need it while looking at the entry
        // because the borrowck sees two mutable refs (list_cache, tree.entry).
        if let Some(list) = list.take() {
            state.list_cache.free(list);
        }

        entry.queued.store(true, Ordering::Relaxed);
        Some(Delay { deadline, entry })
    }

    /// Complete a scheduled Delay by removing it from any Queues and freeing it.
    /// `cancelled` is used as a hint to remove from lists if necessary.
    #[cold]
    pub fn complete(&self, delay: Delay) {
        let mut state = self.state.lock();
        let Delay { deadline, entry } = delay;

        // Remove the entry from any lists if its still scheduled
        if entry.queued.load(Ordering::Relaxed) {
            let mut list = None;
            match state.tree.entry(deadline) {
                TreeEntry::Vacant(_) => unreachable!("Delay entry queued without a list"),
                TreeEntry::Occupied(mut occupied) => {
                    for index in 0..occupied.get().len() {
                        if Arc::ptr_eq(&occupied.get()[index], &entry) {
                            drop(occupied.get_mut().swap_remove(index));
                            break;
                        }
                    }

                    // This was the last entry in the list for the deadline, remove it
                    if occupied.get().len() == 0 {
                        list = Some(occupied.remove());
                    }
                }
            }

            if let Some(list) = list {
                state.list_cache.free(list);

                // There's no longer a list for our deadline.
                // If `expires` was our deadline, find the next smallest deadline to expire at.
                let next_expire = NonZeroU64::new(self.expires.get()).unwrap();
                if next_expire.get() == deadline {
                    let next_deadline = state.tree.iter().next().map(|(deadline, _)| *deadline);
                    self.expires.store(next_deadline.unwrap_or(0));
                }
            }
        }

        // Finally, free the entry
        entry.queued.store(false, Ordering::Relaxed);
        state.entry_cache.free(entry);
    }
}
