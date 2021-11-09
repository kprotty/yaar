use crate::internal::waker::AtomicWaker;
use crate::runtime::scheduler::context::Context;
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

pub type Entry = Arc<RawEntry>;

#[derive(Default)]
pub struct RawEntry {
    pub waker: AtomicWaker,
    queued: AtomicBool,
}

pub type Millis = u64;

pub struct Delay {
    pub deadline: Millis,
    pub entry: Entry,
}

type Queue = Vec<Entry>;

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

#[derive(Default)]
pub struct Expired(Vec<Entry>);

impl Expired {
    pub fn is_empty(&self) -> bool {
        self.0.len() == 0
    }

    pub fn process(&mut self) {
        self.0.drain(..).for_each(|entry| entry.waker.wake());
    }
}

#[derive(Default)]
struct State {
    current: Millis,
    entry_cache: Cache<Entry>,
    queue_cache: Cache<Queue>,
    tree: BTreeMap<Millis, Queue>,
}

pub struct DelayQueue {
    expires: SharedU64,
    state: Mutex<State>,
    started: Instant,
}

impl DelayQueue {
    pub fn new(instant: Instant) -> Self {
        Self {
            expires: SharedU64::default(),
            state: Mutex::new(State::default()),
            started: instant,
        }
    }

    /// Get a reference to the current DelayQueue when running inside a runtime context
    pub fn with<F>(f: impl FnOnce(&Arc<Self>) -> F) -> F {
        let context_ref = Context::current();
        let context = context_ref.as_ref();

        let worker_index = context.worker_index.get().unwrap_or_else(|| {
            let rng = context.rng.borrow_mut().gen();
            rng % context.executor.workers.len()
        });

        f(&context.executor.workers[worker_index].delay_queue)
    }

    /// Convert an Instant into u64 millis using the Delay's Instant as the starting epoch
    pub fn since(&self, instant: Instant) -> Millis {
        instant
            .checked_duration_since(self.started)
            .map(|duration| duration.as_millis().try_into().unwrap_or(Millis::MAX))
            .unwrap_or(0)
    }

    /// Peek the next millis deadline that the DelayQueue will expire at
    pub fn expires(&self) -> Option<Millis> {
        NonZeroU64::new(self.expires.load()).map(|e| e.get())
    }

    /// Update the DelayQueue to the `current` time and expire any timer Entries into `expired`.
    pub fn poll(&self, current: Millis, expired: &mut Expired) {
        let mut state = self.state.lock();
        state.current = state.current.max(current);

        while let Some(deadline) = state.tree.iter().map(|i| *i.0).next() {
            if deadline > state.current {
                self.expires.store(deadline);
                return;
            }

            let mut queue = state.tree.remove(&deadline).unwrap();
            let entries = queue.drain(..).map(|entry| {
                assert!(entry.queued.load(Ordering::Relaxed));
                entry.queued.store(false, Ordering::Relaxed);
                entry
            });

            expired.0.extend(entries);
            state.queue_cache.free(queue);
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
                    .map(|millis: Millis| millis.max(1))
                    .and_then(|m| m.checked_add(state.current))
                    .unwrap_or(Millis::MAX)
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
        let mut queue = Some(state.queue_cache.alloc());

        match state.tree.entry(deadline) {
            TreeEntry::Occupied(ref mut occupied) => occupied.get_mut().push(entry.clone()),
            TreeEntry::Vacant(vacant) => {
                let queue = queue.take().unwrap();
                vacant.insert(queue).push(entry.clone());
            }
        }

        // A new Queue wasn't needed so free it.
        // We can't allocate a Queue when we need it while looking at the entry
        // because the borrowck sees two mutable refs (queue_cache, tree.entry).
        if let Some(queue) = queue.take() {
            state.queue_cache.free(queue);
        }

        entry.queued.store(true, Ordering::Relaxed);
        Some(Delay { deadline, entry })
    }

    /// Complete a scheduled Delay by removing it from any Queues and freeing it.
    /// `cancelled` is used as a hint to remove from queues if necessary.
    #[cold]
    pub fn complete(&self, delay: Delay) {
        let mut state = self.state.lock();
        let Delay { deadline, entry } = delay;

        // Remove the entry from any queues if its still scheduled
        if entry.queued.load(Ordering::Relaxed) {
            let mut queue = None;
            match state.tree.entry(deadline) {
                TreeEntry::Vacant(_) => unreachable!("Delay entry queued without a queue"),
                TreeEntry::Occupied(mut occupied) => {
                    for index in 0..occupied.get().len() {
                        if Arc::ptr_eq(&occupied.get()[index], &entry) {
                            drop(occupied.get_mut().swap_remove(index));
                            break;
                        }
                    }

                    // This was the last entry in the queue for the deadline, remove it
                    if occupied.get().len() == 0 {
                        queue = Some(occupied.remove());
                    }
                }
            }

            if let Some(queue) = queue {
                state.queue_cache.free(queue);

                // There's no longer a queue for our deadline.
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

/// A SharedU64 is an AtomicU64 that only supports load() and store().
/// Many threads can call load() but only a single thread may call store() and get().
/// It's meant to be fast to access on all platforms given its simple requirements.
use shared_u64::SharedU64;

// On 64bit platforms, use a regular AtomicU64.
#[cfg(target_pointer_width = "64")]
mod shared_u64 {
    use std::sync::atomic::{AtomicU64, Ordering};

    #[derive(Default)]
    pub struct SharedU64(AtomicU64);

    impl SharedU64 {
        pub fn get(&self) -> u64 {
            self.0.load(Ordering::Relaxed)
        }

        pub fn load(&self) -> u64 {
            self.0.load(Ordering::Acquire)
        }

        pub fn store(&self, value: u64) {
            self.0.store(value, Ordering::Relaxed);
        }
    }
}

/// On 32bit platforms, use a trick also found in windows KSYSTEM_TIME accesses.
///
/// It works by using AtomicU32s which are native and having a copy of the u64 high bits.
/// It must write the high bits copy first, then low bits, then real high bits in that order.
/// Readers must read them in strictly reverse order: real high bits, low bits, high bits copy.
/// If the high bits copy matches the real high bits, then the Reader read a valid u64.
///
/// https://wrkhpi.wordpress.com/2007/08/09/getting-os-information-the-kuser_shared_data-structure/
#[cfg(target_pointer_width = "32")]
mod shared_u64 {
    use std::{
        convert::TryInto,
        sync::atomic::{AtomicU32, Ordering},
    };

    #[derive(Default)]
    pub struct SharedU64 {
        low: AtomicU32,
        high: AtomicU32,
        high2: AtomicU32,
    }

    impl SharedU64 {
        pub fn get(&self) -> u64 {
            let high = self.high.load(Ordering::Relaxed);
            let low = self.low.load(Ordering::Relaxed);
            ((high as u64) << 32) | low
        }

        pub fn load(&self) -> u64 {
            loop {
                let high = self.high.load(Ordering::Acquire);
                let low = self.low.load(Ordering::Acquire);
                let high2 = self.high2.load(Ordering::Relaxed);

                if high == high2 {
                    return ((high as u64) << 32) | low;
                }
            }
        }

        pub fn store(&self, value: u64) {
            let high: u32 = (value >> 32).try_into().unwrap();
            let low: u32 = (value & 0xffffffff).try_into().unwrap();

            self.high2.store(high, Ordering::Relaxed);
            self.low.store(low, Ordering::Release);
            self.high.store(high, Ordering::Release);
        }
    }
}
