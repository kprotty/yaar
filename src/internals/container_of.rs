macro_rules! container_of {
    ($type:ty, $field:ident, $field_expr:expr) => {{
        let uninit = std::mem::MaybeUninit::<$type>::uninit();
        let field_ptr = std::ptr::addr_of!((*uninit.as_ptr()).$field);
        let field_offset = (field_ptr as usize) - (uninit.as_ptr() as usize);
        std::ptr::NonNull::new_unchecked(((field_expr as usize) - field_offset) as *mut $type)
    }};
}
