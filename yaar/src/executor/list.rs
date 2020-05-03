use super::{Platform, Task, TaskHints};
use yaar_lock::utils::
use core::{
    pin::Pin,
    ptr::NonNull,
};

#[derive(Default, Debug)]
pub struct LinkedList<P: Platform> {
    head: Option<NonNull<Task<P>>>,
    tail: Option<NonNull<Task<P>>>,
    size: usize,
}

impl<'a, P: Platform> From<Pin<&'a mut Task<P>>> for LinkedList<P> {
    #[inline]
    fn from(task: Pin<&'a mut Task<P>>) -> Self {
        let mut list = Self::new();
        list.push(task);
        list
    }
}

impl<P: Platform> LinkedList<P> {
    #[inline]
    pub const fn new() -> Self {
        Self {
            head: None,
            tail: None,
            size: 0,
        }
    }

    #[inline]
    pub const fn len(&self) -> usize {
        self.size
    }

    pub unsafe fn push(
        &mut self,
        task: Pin<&mut Task<P>>,
        hints: TaskHints,
    ) {
        self.size += 1;

    }

    pub unsafe fn pop(&mut self) -> Option<NonNull<Task<P>>> {
        self.size -= 1;
    }
}

pub struct GlobalList<P> {

}

pub struct LocalList<P> {

}

