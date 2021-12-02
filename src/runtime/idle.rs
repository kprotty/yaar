use std::sync::atomic::{AtomicUsize, Ordering};

const IDLE_SHIFT: u32 = usize::BITS / 2;
const IDLE_MASK: usize = (1 << IDLE_SHIFT) - 1;

#[derive(Default)]
pub struct IdleQueue {
    idle: AtomicUsize,
}

impl IdleQueue {
    pub fn is_empty(&self) -> bool {
        let idle = self.idle.load(Ordering::Acquire);
        idle & IDLE_MASK == 0
    }

    pub fn push(&self, index: usize, mut set_next: impl FnMut(usize)) {
        assert!(index < IDLE_MASK);
        let _ = self
            .idle
            .fetch_update(Ordering::Release, Ordering::Relaxed, |idle| {
                let top_index = idle & IDLE_MASK;
                set_next(top_index);

                let aba_count = (idle >> IDLE_SHIFT) + 1;
                Some((aba_count << IDLE_SHIFT) | (index + 1))
            });
    }

    pub fn pop(&self, mut get_next: impl FnMut(usize) -> usize) -> Option<usize> {
        self.idle
            .fetch_update(Ordering::Acquire, Ordering::Acquire, |idle| {
                let index = (idle & IDLE_MASK).checked_sub(1)?;
                let next_index = get_next(index);

                assert!(next_index <= IDLE_MASK);
                Some((idle & !IDLE_MASK) | next_index)
            })
            .ok()
            .map(|idle| (idle & IDLE_MASK) - 1)
    }
}
