use super::{Node, Thread, Platform};
use core::{
    cell::Cell,
    ptr::NonNull,
};

pub struct Worker<P: Platform> {
    pub data: P::WorkerLocalData,
    pub(crate) next: Cell<Option<NonNull<Self>>>,
    pub(crate) node: Cell<Option<NonNull<Node<P>>>>,
    pub(crate) thread: Cell<Option<NonNull<Thread<P>>>>,
}

impl<P: Platform> Worker<P> {
    pub fn node(&self) -> Option<&Node<P>> {
       self.node.get().map(|ptr| unsafe { &*ptr.as_ptr() })
    }

    pub fn thread(&self) -> Option<&Thread<P>> {
        self.thread.get().map(|ptr| unsafe { &*ptr.as_ptr() })
    }
}