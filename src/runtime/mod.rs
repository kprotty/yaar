
pub mod task;

#[cfg(feature = "rt-serial")]
pub mod serial;

#[cfg(feature = "alloc")]
pub struct DefaultAllocator;

#[cfg(feature = "alloc")]
unsafe impl core::alloc::GlobalAlloc for DefaultAllocator {
    unsafe fn alloc(&self, layout: core::alloc::Layout) -> *mut u8 {
        alloc::alloc::alloc(layout)
    }

    unsafe fn dealloc(&self, ptr: *mut u8, layout: core::alloc::Layout) {
        alloc::alloc::dealloc(ptr, layout)
    }

    unsafe fn alloc_zeroed(&self, layout: core::alloc::Layout) -> *mut u8 {
        alloc::alloc::alloc_zeroed(layout)
    }

    unsafe fn realloc(&self, ptr: *mut u8, layout: core::alloc::Layout, new_size: usize) -> *mut u8 {
        alloc::alloc::realloc(ptr, layout, new_size)
    }
}