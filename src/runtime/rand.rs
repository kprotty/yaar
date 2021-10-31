use gcd::Gcd;
use std::{mem::size_of, num::NonZeroUsize};

#[derive(Copy, Clone)]
pub struct RandomIterGen {
    range: NonZeroUsize,
    co_prime: NonZeroUsize,
}

impl From<NonZeroUsize> for RandomIterGen {
    fn from(range: NonZeroUsize) -> Self {
        Self {
            range,
            co_prime: match range.get() {
                1 => range,
                range => ((range / 2)..range)
                    .rev()
                    .filter(|r| r.gcd(range) == 1)
                    .next()
                    .and_then(NonZeroUsize::new)
                    .unwrap(),
            },
        }
    }
}

pub struct RandomSource {
    xorshift: usize,
}

impl RandomSource {
    pub fn new(seed: usize) -> Self {
        #[cfg(target_pointer_width = "64")]
        const HASH: usize = 0x9E3779B97F4A7C15;

        #[cfg(target_pointer_width = "32")]
        const HASH: usize = 0x9E3779B9;

        Self {
            xorshift: NonZeroUsize::new(seed)
                .map(|seed| seed.get())
                .unwrap_or(0xdeadbeef)
                .wrapping_mul(HASH),
        }
    }

    pub fn next(&mut self) -> usize {
        let shifts = match size_of::<usize>() {
            8 => (13, 7, 17),
            4 => (13, 17, 5),
            _ => unreachable!("architecture unsupported"),
        };

        self.xorshift ^= self.xorshift << shifts.0;
        self.xorshift ^= self.xorshift >> shifts.1;
        self.xorshift ^= self.xorshift << shifts.2;
        self.xorshift
    }

    pub fn iter(&mut self, gen: RandomIterGen) -> impl Iterator<Item = usize> {
        let range = gen.range.get();
        let prime = gen.co_prime.get();
        let mut rng = self.next() % range;

        (0..range).map(move |_| {
            rng += prime;
            if rng >= range {
                rng -= range;
            }

            assert!(rng < range);
            rng
        })
    }
}
