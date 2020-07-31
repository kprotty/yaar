// Copyright 2019-2020 kprotty
//
// Licensed under the Apache License, Version 2.0, <LICENSE-APACHE or
// http://apache.org/licenses/LICENSE-2.0> or the MIT license <LICENSE-MIT or
// http://opensource.org/licenses/MIT>, at your option. This file may not be
// copied, modified, or distributed except according to those terms.

use super::{Task, TaskBatch};
use core::{
    fmt,
    pin::Pin,
    ptr::NonNull,
    marker::PhantomPinned,
    sync::atomic::{AtomicUsize, AtomicPtr, Ordering},
};

#[derive(Default)]
pub struct GlobalQueue {
    _pinned: PhantomPinned,
    head: AtomicPtr<Task>,
    tail: AtomicUsize,
    stub: AtomicPtr<Task>,
}

unsafe impl Send for GlobalQueue {}
unsafe impl Sync for GlobalQueue {}

impl fmt::Debug for GlobalQueue {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("GlobalQueue")
            .field("is_empty", &self.is_empty())
            .finish()
    }
}

impl GlobalQueue {
    pub fn init(self: Pin<&mut Self>) {

    }

    #[inline]
    fn stub_ptr(&self) -> NonNull<Task> {
        NonNull::new(&self.stub as *const _ as *mut Task).unwrap()
    }

    fn is_empty(&self) -> bool {

    }

    pub fn push(&self, batch: TaskBatch) {

    }

    fn try_pop(&self) -> Option<GlobalQueueReceiver<'_>> {

    }
}

struct GlobalQueueReceiver<'a> {
    queue: &'a GlobalQueue,
    tail: NonNull<Task>,
}

impl<'a> Iterator for GlobalQueueReceiver<'a> {
    type Item = NonNull<Task>;

    fn next(&mut self) -> Option<Self::Item> {
        
    }
}

pub struct LocalQueue {
    next: AtomicPtr<Task>,
    head: AtomicUsize,
    tail: AtomicUsize,
    buffer: [MaybeUninit<NonNull<Task>>; Self::BUFFER_SIZE],
}

impl fmt::Debug for LocalQueue {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("LocalQueue")
            .field("size", &self.len())
            .finish()
    }
}

impl Default for LocalQueue {
    fn default() -> Self {

    }
}

impl LocalQueue {
    const BUFFER_SIZE: usize = 256;

    fn try_push(&self, mut batch: TaskBatch) -> Result<(), TaskBatch> {

    }

    fn try_pop(&self) -> Option<NonNull<Task>> {

    }

    fn try_steal_from_local(&self, target: &Self) -> Option<NonNull<Task>> {

    }

    fn try_steal_from_global(&self, target: &GlobalQueue) -> Option<NonNull<Task>> {
        
    }
}