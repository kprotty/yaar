use crate::internal::waker::AtomicWaker;
use crate::runtime::scheduler::context::Context;
use parking_lot::Mutex;
use std::{
    collections::btree_map::Entry as TreeEntry,
    collections::BTreeMap,
    mem::drop,
    num::NonZeroU64,
    sync::Arc,
    time::{Duration, Instant},
};

pub type Millis = u64;

pub type Entry = Arc<AtomicWaker>;

pub struct Delay {
    pub deadline: Millis,
    pub entry: Entry,
}

type Queue = Vec<Entry>;

#[derive(Default)]
struct Cache<T>(Vec<T>);

impl<T> Cache<T> {
    fn len(&self) -> usize {
        self.0.len()
    }

    fn push(&mut self, item: T) {
        self.0.push(item)
    }

    fn pop(&mut self) -> Option<T> {
        let index = self.len().checked_sub(1);
        index.map(|i| self.0.swap_remove(i))
    }
}

#[derive(Default)]
pub struct Expired(Vec<Entry>);

impl Expired {
    pub fn is_empty(&self) -> bool {
        self.0.len() == 0
    }

    pub fn process(&mut self) {
        self.0.drain(..).for_each(|entry| entry.wake());
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

    pub fn with<F>(f: impl FnOnce(&Arc<Self>) -> F) -> F {
        let context_ref = Context::current();
        let context = context_ref.as_ref();

        let worker_index = context.worker_index.get().unwrap_or_else(|| {
            let rng = context.rng.borrow_mut().gen();
            rng % context.executor.workers.len()
        });

        f(&context.executor.workers[worker_index].delay_queue)
    }

    pub fn since(&self, instant: Instant) -> Millis {
        instant
            .checked_duration_since(self.started)
            .map(|duration| duration.as_millis().try_into().unwrap_or(Millis::MAX))
            .unwrap_or(0)
    }

    pub fn expires(&self) -> Option<Millis> {
        NonZeroU64::new(self.expires.load()).map(|e| e.get())
    }

    pub fn poll(&self, current: Millis, expired: &mut Expired) {
        let mut state = self.state.lock();
        state.current = state.current.max(current);

        while let Some(deadline) = state.tree.iter().map(|i| *i.0).next() {
            if deadline > state.current {
                self.expires.store(deadline);
                return;
            }

            let mut queue = state.tree.remove(&deadline).unwrap();
            expired.0.extend(queue.drain(..));

            if state.queue_cache.len() < 64 {
                state.queue_cache.push(queue);
            }
        }

        self.expires.store(0);
    }

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

        match NonZeroU64::new(self.expires.get()) {
            Some(e) if deadline < e.get() => self.expires.store(deadline),
            Some(_) => {}
            None => self.expires.store(deadline),
        }

        let entry = state
            .entry_cache
            .pop()
            .unwrap_or_else(|| Entry::new(AtomicWaker::new()));

        let mut queue_cached = true;
        let mut queue = state.queue_cache.pop().or_else(|| {
            queue_cached = false;
            Some(Queue::new())
        });

        match state.tree.entry(deadline) {
            TreeEntry::Occupied(ref mut occupied) => occupied.get_mut().push(entry.clone()),
            TreeEntry::Vacant(vacant) => {
                let queue = queue.take().unwrap();
                vacant.insert(queue).push(entry.clone());
            }
        }

        if let Some(queue) = queue.take() {
            if queue_cached {
                state.queue_cache.push(queue);
            }
        }

        Some(Delay { deadline, entry })
    }

    #[cold]
    pub fn complete(&self, delay: Delay, cancelled: bool) {
        let Delay { deadline, entry } = delay;

        let mut state = self.state.lock();
        if entry.poll(None).is_ready() {
            return;
        }

        let mut removed = None;
        if cancelled {
            if let TreeEntry::Occupied(mut occupied) = state.tree.entry(deadline) {
                for index in 0..occupied.get().len() {
                    if Arc::ptr_eq(&occupied.get()[index], &entry) {
                        drop(occupied.get_mut().swap_remove(index));
                        break;
                    }
                }

                if occupied.get().len() == 0 {
                    removed = Some(occupied.remove());
                }
            }
        }

        if let Some(queue) = removed {
            if state.queue_cache.len() < 64 {
                state.queue_cache.push(queue);
            }

            let next_expire = NonZeroU64::new(self.expires.get()).unwrap();
            if next_expire.get() == deadline {
                let next_deadline = state.tree.iter().next().map(|(deadline, _)| *deadline);
                self.expires.store(next_deadline.unwrap_or(0));
            }
        }

        if state.entry_cache.len() < 64 {
            return state.entry_cache.push(entry);
        }

        drop(state);
        drop(entry);
    }
}

use shared_u64::SharedU64;

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
