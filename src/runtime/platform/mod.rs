
use core::num::NonZeroUsize;

pub trait Platform {
    /// Hint to the system to yield the current thread of execution
    fn yield_now();

    /// The minimum size in bytes for custom thread stacks
    fn min_thread_stack_size() -> usize;

    /// Get the number of logical cpus present on the system, returning None on error.
    fn num_cpus() -> Option<NonZeroUsize>;
}

#[cfg(all(unix, feature = "os-platform"))]
mod unix;
#[cfg(all(unix, feature = "os-platform"))]
pub use unix::OsPlatform;

#[cfg(all(windows, feature = "os-platform"))]
mod windows;
#[cfg(all(windows, feature = "os-platform"))]
pub use windows::OsPlatform;

#[cfg(feature = "std")]
pub use self::std_allocator::*;
#[cfg(feature = "std")]
mod std_allocator {
    use core::alloc::{Layout, GlobalAlloc};
    
    #[derive(Default, Copy, Clone)]
    pub struct StdAllocator;

    unsafe impl GlobalAlloc for StdAllocator {
        unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
            std::alloc::alloc(layout)
        }
    
        unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
            std::alloc::dealloc(ptr, layout)
        }
    }
}
