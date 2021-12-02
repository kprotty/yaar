use std::{
    mem::replace,
    num::NonZeroUsize,
    sync::atomic::{AtomicUsize, Ordering},
};

#[derive(Copy, Clone)]
pub struct RngSeqSeed {
    prime: NonZeroUsize,
    range: NonZeroUsize,
}

impl RngSeqSeed {
    pub fn new(range: NonZeroUsize) -> Self {
        let prime = ((range.get() / 2)..range.get())
            .rev()
            .filter(|&n| Self::gcd(n, range.get()) == 1)
            .next()
            .and_then(NonZeroUsize::new)
            .unwrap();

        Self { prime, range }
    }

    fn gcd(mut a: usize, mut b: usize) -> usize {
        while a != b {
            if a > b {
                a -= b;
            } else {
                b -= a;
            }
        }
        a
    }
}

pub struct Rng {
    xorshift: NonZeroUsize,
}

impl Rng {
    pub fn new() -> Self {
        #[cfg(target_pointer_width = "64")]
        const FIB_HASH: usize = 0x9E3779B97F4A7C15;
        #[cfg(target_pointer_width = "32")]
        const FIB_HASH: usize = 0x9E3779B9;

        static RNG_SEED: AtomicUsize = AtomicUsize::new(0);
        let seed = RNG_SEED.fetch_add(1, Ordering::Relaxed);
        let seed = seed.wrapping_mul(FIB_HASH);

        Self {
            xorshift: NonZeroUsize::new(seed)
                .or(NonZeroUsize::new(0xdeadbeef))
                .unwrap(),
        }
    }

    pub fn next(&mut self) -> usize {
        let shifts = match usize::BITS {
            64 => (13, 7, 17),
            32 => (13, 17, 5),
            _ => unreachable!("platform not supported"),
        };

        let mut xs = self.xorshift.get();
        xs ^= xs << shifts.0;
        xs ^= xs >> shifts.0;
        xs ^= xs << shifts.0;

        self.xorshift = NonZeroUsize::new(xs).unwrap();
        xs
    }

    pub fn seq(&mut self, seed: RngSeqSeed) -> impl Iterator<Item = usize> {
        let mut index = self.next() % seed.range.get();

        (0..seed.range.get()).map(move |_| {
            let mut new_index = index + seed.prime.get();
            if new_index >= seed.range.get() {
                new_index -= seed.range.get();
            }

            assert!(new_index < seed.range.get());
            replace(&mut index, new_index)
        })
    }
}
