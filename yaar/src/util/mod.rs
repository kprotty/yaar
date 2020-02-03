#[macro_export]
macro_rules! field_parent_ptr {
    ($Type:ty, $field:ident, $field_ptr:expr) => {{
        let stub = core::mem::MaybeUninit::<$Type>::zeroed();
        let base = stub.as_ptr() as usize;
        let field = &(*stub.as_ptr()).$field as *const _ as usize;
        ((($field_ptr) as usize) - (field - base)) as *mut $Type
    }};
}

mod cache_padded;
pub use self::cache_padded::*;
