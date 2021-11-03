use gcd::Gcd;
use std::{mem::size_of, num::NonZeroUsize};

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
                .map(|prime| prime.gcd(range.get()))
                .filter_map(NonZeroUsize::new)
                .next()
                .unwrap(),
        }
    }
}

pub struct RandomGenerator {
    xorshift: NonZeroUsize,
}

impl From<usize> for RandomGenerator {
    fn from(seed: usize) -> Self {
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
}

impl RandomGenerator {
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
        let mut offset = self.gen();

        (0..iter_source.range.get()).map(move |_| {
            offset += iter_source.prime.get();
            if offset >= iter_source.range.get() {
                offset -= iter_source.range.get();
            }

            let index = offset;
            assert!(index < iter_source.range.get());
            index
        })
    }
}
