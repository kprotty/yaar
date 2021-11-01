use std::{
    num::NonZeroUsize,
    sync::atomic::{AtomicUsize, Ordering},
};

static CPU_COUNT: AtomicUsize = AtomicUsize::new(0);

#[inline]
pub fn get() -> NonZeroUsize {
    NonZeroUsize::new(CPU_COUNT.load(Ordering::Relaxed)).unwrap_or_else(|| get_slow())
}

#[cold]
fn get_slow() -> NonZeroUsize {
    let cpu_count = NonZeroUsize::new(num_cpus::get())
        .or(NonZeroUsize::new(1))
        .unwrap();

    CPU_COUNT.store(cpu_count, Ordering::Relaxed);
    cpu_count
}
