use gcd::Gcd;
use std::{
    mem::size_of,
    num::NonZeroUsize,
    sync::atomic::{AtomicUsize, Ordering},
};

#[derive(Copy, Clone)]
pub struct RandomIterSource {
    range: NonZeroUsize,
    prime: NonZeroUsize,
}

impl From<NonZeroUsize> for RandomIterSource {
    fn from(range: NonZeroUsize) -> Self {
        Self {
            range,
            prime: ((range.get() / 2)..range.get())
                .rev()
                .filter(|prime| prime.gcd(range.get()) == 1)
                .filter_map(NonZeroUsize::new)
                .next()
                .unwrap(),
        }
    }
}

pub struct RandomGenerator {
    xorshift: NonZeroUsize,
}

impl RandomGenerator {
    pub fn new() -> Self {
        static SEED: AtomicUsize = AtomicUsize::new(0);
        let seed = SEED.fetch_add(1, Ordering::Relaxed);

        #[cfg(target_pointer_width = "64")]
        const HASH: usize = 0x9E3779B97F4A7C15;
        #[cfg(target_pointer_width = "32")]
        const HASH: usize = 0x9E3779B9;

        Self {
            xorshift: NonZeroUsize::new(seed.wrapping_mul(HASH))
                .or(NonZeroUsize::new(0xdeadbeef))
                .unwrap(),
        }
    }

    pub fn gen(&mut self) -> usize {
        let shifts = match size_of::<usize>() {
            8 => (13, 7, 17),
            4 => (13, 17, 5),
            _ => unreachable!(),
        };

        let mut xs = self.xorshift.get();
        xs ^= xs << shifts.0;
        xs ^= xs >> shifts.1;
        xs ^= xs << shifts.2;

        self.xorshift = NonZeroUsize::new(xs).unwrap();
        xs
    }

    pub fn gen_iter(&mut self, iter_source: RandomIterSource) -> impl Iterator<Item = usize> {
        let range = iter_source.range.get();
        let prime = iter_source.prime.get();
        let mut index = self.gen() % range;

        (0..range).map(move |_| {
            index += prime;
            if index >= range {
                index -= range;
            }

            assert!(index < range);
            index
        })
    }
}
