use core::{ptr, sync::atomic};

pub trait Unsync: Sync {
    type Output;

    unsafe fn load_unsync(&self) -> Self::Output;
    unsafe fn store_unsync(&self, value: Self::Output);
}

#[cfg_attr(feature = "nightly", target_has_atomic = "ptr")]
impl Unsync for atomic::AtomicUsize {
    type Output = usize;

    #[inline]
    unsafe fn load_unsync(&self) -> Self::Output {
        ptr::read_volatile(self as *const _ as *const Self::Output)
    }

    #[inline]
    unsafe fn store_unsync(&self, value: Self::Output) {
        ptr::write_volatile(self as *const _ as *mut Self::Output, value)
    }
}
