/// A SharedU64 is an AtomicU64 that only supports load() and store().
/// Many threads can call load() but only a single thread may call store() and get().
/// It's meant to be fast to access on all platforms given its simple requirements.
pub use shared_u64::SharedU64;

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
