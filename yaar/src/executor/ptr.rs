use core::{
    mem::align_of,
    ptr::{null, NonNull},
    marker::PhantomData,
};

pub struct TaggedPtr<Ptr, Tag> {
    value: usize,
    phantom: PhantomData<*mut (Ptr, Tag)>,
}

impl<Ptr, Tag> Copy for TaggedPtr<Ptr, Tag> {}
impl<Ptr, Tag> Clone for TaggedPtr<Ptr, Tag> {
    fn clone(&self) -> Self {
        Self::from(self.value)
    }
}

impl<Ptr, Tag> Into<usize> for TaggedPtr<Ptr, Tag> {
    fn into(self) -> usize {
        self.value
    }
}

impl<Ptr, Tag> From<usize> for TaggedPtr<Ptr, Tag> {
    fn from(value: usize) -> Self {
        Self {
            value,
            phantom: PhantomData,
        }
    }
}

impl<Ptr, Tag> TaggedPtr<Ptr, Tag>
where
    Tag: From<usize> + Into<usize>,
{
    pub fn new(ptr: *const Ptr, tag: Tag) -> Self {
        let tag = tag.into();
        debug_assert!(align_of::<Ptr>() > tag);
        ((ptr as usize) | tag).into()
    }

    pub fn as_ptr(self) -> Option<NonNull<Ptr>> {
        NonNull::new((self.value & !(align_of::<Ptr>() - 1)) as *mut Ptr)
    }

    pub fn as_tag(self) -> Tag {
        Tag::from(self.value & (align_of::<Ptr>() - 1))
    }

    pub fn with_ptr(self, ptr: Option<NonNull<Ptr>>) -> Self {
        let ptr = ptr.map(|p| p.as_ptr() as *const _).unwrap_or(null());
        Self::new(ptr, self.as_tag())
    }

    pub fn with_tag(self, tag: Tag) -> Self {
        let ptr = self
            .as_ptr()
            .map(|p| p.as_ptr() as *const _)
            .unwrap_or(null());
        Self::new(ptr, tag)
    }
}