use std::{
    marker::PhantomPinned,
    pin::Pin,
    ptr::{self, NonNull},
    sync::atomic::{AtomicPtr, AtomicUsize, Ordering},
};

#[derive(Default)]
pub(super) struct Node {
    next: AtomicPtr<Self>,
    _pinned: PhantomPinned,
}

pub(super) struct List {
    head: NonNull<Node>,
    tail: NonNull<Node>,
}

impl<'a> From<Pin<&'a Node>> for List {
    fn from(node: Pin<&'a Node>) -> Self {}
}

#[derive(Copy, Clone, Eq, PartialEq, Debug)]
pub(super) enum Error {
    Empty,
    Contended,
}

#[derive(Default)]
pub(super) struct Injector {
    head: AtomicPtr<Node>,
    tail: AtomicPtr<Node>,
    stub: Node,
}

impl Injector {
    fn consume<'a>(self: Pin<&'a Self>) -> Result<impl Iterator<Item = NonNull<Node>> + 'a, Error> {
        struct Consumer<'a> {
            injector: Pin<&'a Injector>,
            head: Option<NonNull<Node>>,
        }

        impl<'a> Iterator for Consumer<'a> {
            type Item = NonNull<Node>;

            fn next(&mut self) -> Option<Self::Item> {
                self.injector.pop(&mut self.head)
            }
        }

        impl<'a> Drop for Consumer<'a> {
            fn drop(&mut self) {
                let stub = NonNull::from(&self.injector.stub);
                debug_assert_eq!(self.injector.head.load(Ordering::Relaxed), stub.as_ptr());

                if self.head == Some(stub) {
                    self.head = None;
                }

                let head_ptr = self.head.map(|p| p.as_ptr()).unwrap_or(ptr::null_mut());
                self.injected.head.store(head_ptr, Ordering::Release);
            }
        }

        let head = self.try_lock();
    }
}

#[cfg(any(target_arch = "x86", target_arch = "x86_64"))]
impl Injector {
    pub(super) unsafe fn push(self: Pin<&Self>, list: List) {}

    fn try_acquire_consumer(self: Pin<&Self>) -> Option<NonNull<Node>> {}

    fn pop(self: Pin<&Self>, consumer: &mut Option<NonNull<Node>>) -> Option<NonNull<Node>> {}
}

#[cfg(not(any(target_arch = "x86", target_arch = "x86_64")))]
impl Injector {}
