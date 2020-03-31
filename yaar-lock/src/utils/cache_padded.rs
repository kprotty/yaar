use core::{
    fmt,
    ops::{Deref, DerefMut},
};

#[cfg_attr(target_arch = "x86_64", repr(align(128)))]
#[cfg_attr(not(target_arch = "x86_64"), repr(align(64)))]
#[derive(Copy, Clone, Eq, PartialEq, Default, Hash)]
pub struct CachePadded<T> {
    value: T,
}

impl<T> From<T> for CachePadded<T> {
    fn from(value: T) -> Self {
        Self::new(value)
    }
}

impl<T: fmt::Debug> fmt::Debug for CachePadded<T> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("CachePadded")
            .field("value", &self.value)
            .finish()
    }
}

impl<T> Deref for CachePadded<T> {
    type Target = T;

    fn deref(&self) -> &T {
        &self.value
    }
}

impl<T> DerefMut for CachePadded<T> {
    fn deref_mut(&mut self) -> &mut T {
        &mut self.value
    }
}

impl<T> CachePadded<T> {
    pub const fn new(value: T) -> Self {
        Self { value }
    }

    pub fn into_inner(self) -> T {
        self.value
    }
}
