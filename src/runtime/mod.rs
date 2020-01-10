
pub mod task;

#[cfg(feature = "rt-serial")]
pub mod serial;

#[cfg(feature = "alloc")]
pub use self::default_allocator::*;

#[cfg(feature = "alloc")]
mod default_allocator {
    use core::alloc::{Layout, Global};
    
    pub struct DefaultAllocator;

    unsafe impl GlobalAlloc for DefaultAllocator {
        unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
            alloc::alloc::alloc(layout)
        }
    
        unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
            alloc::alloc::dealloc(ptr, layout)
        }
    
        unsafe fn alloc_zeroed(&self, layout: Layout) -> *mut u8 {
            alloc::alloc::alloc_zeroed(layout)
        }
    
        unsafe fn realloc(&self, ptr: *mut u8, layout: Layout, new_size: usize) -> *mut u8 {
            alloc::alloc::realloc(ptr, layout, new_size)
        }
    }
}
