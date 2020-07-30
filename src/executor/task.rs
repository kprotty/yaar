// Copyright 2019-2020 kprotty
//
// Licensed under the Apache License, Version 2.0, <LICENSE-APACHE or
// http://apache.org/licenses/LICENSE-2.0> or the MIT license <LICENSE-MIT or
// http://opensource.org/licenses/MIT>, at your option. This file may not be
// copied, modified, or distributed except according to those terms.

use super::Thread;
use core::{
    fmt,
    marker::PhantomPinned,
    pin::Pin,
    ptr::{self, NonNull},
    sync::atomic::AtomicPtr,
};

/// A task callback is the continuation that is executed when
/// a task is running on a given thread in the scheduler.
/// Once a task is scheduled onto a scheduler, this is the function that is
/// invoked.
///
/// The first argument is a reference to itself which, in reality, is a Pin<&mut
/// Task>. Passing in the self reference acts as context for what the task was
/// scheduled to accomplish.
///
/// The second argument is conceptually a Pin<&Thread> which is the current
/// [`Thread`] the task is running on. The Thread reference is the interface for
/// the Task to communicate with the scheduler. Common interfacing functionality
/// include querying the scheduler topology and scheduling more Tasks.
///
/// Finally, the callback takes raw pointers and is an extern function in order
/// to be compatible with C. One of the goals of yaar is to allow using the task
/// scheduler as a shared library.
pub type TaskCallback = extern "C" fn(*mut Task, *const Thread);

/// A task represents a unit of concurrency within a scheduler.
/// This is to say that each task may be executed independent of other running
/// tasks.
///
/// Tasks are intrusively provided by the caller so that the scheduler remains
/// allocation free. They also contain a minimal amount of state needed to
/// represent a computation that can be scheduled in a lock-free manner.
#[repr(C)]
pub struct Task {
    _pinned: PhantomPinned,
    next: AtomicPtr<Self>,
    callback: TaskCallback,
}

impl fmt::Debug for Task {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Task").finish()
    }
}

impl From<TaskCallback> for Task {
    fn from(callback: TaskCallback) -> Self {
        Self {
            _pinned: PhantomPinned,
            next: AtomicPtr::default(),
            callback,
        }
    }
}

impl Task {
    /// Create a new task using the callback provided
    #[cfg(feature = "nightly")]
    pub const fn new(callback: TaskCallback) -> Self {
        Self {
            _pinned: PhantomPinned,
            next: AtomicPtr::new(ptr::null_mut()),
            callback,
        }
    }

    /// Execute the callback this task was created with using the given
    /// [`Thread`] as context.
    ///
    /// # Safety:
    ///
    /// The mutable reference indicates that the caller is the only owner of the
    /// task. However, the callback itself is an extern function which
    /// requires the caller to ensure that the callback being invoked is
    /// actually safe.
    #[inline]
    pub unsafe fn run(self: Pin<&mut Self>, thread: Pin<&Thread>) {
        let mut_self = Pin::get_unchecked_mut(self);
        (mut_self.callback)(mut_self, &*thread)
    }
}

/// A task batch represents an ordered collection of tasks that can be scheduled
/// together.
///
/// # Safety:
///
/// Once a task is enqueued onto a TaskBatch, it must not be invalidated or
/// modified until it is either dequeued from the TaskBatch or the TaskBatch is
/// dropped. Enqueue'ing a Task into a TaskBatch, running a task, and scheduling
/// a Task count as forms of modification.
#[repr(C)]
pub struct TaskBatch {
    head: Option<NonNull<Task>>,
    tail: NonNull<Task>,
    size: usize,
}

impl fmt::Debug for TaskBatch {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("TaskBatch")
            .field("is_empty", &(self.size == 0))
            .finish()
    }
}

impl From<Pin<&mut Task>> for TaskBatch {
    fn from(task: Pin<&mut Task>) -> Self {
        // Convert the task into a NonNull<Task> to use for TaskBatch operations.
        // Safety: upholds the Pin contract as the task reference is never invalidated.
        let task = unsafe {
            let mut task = NonNull::from(Pin::get_unchecked_mut(task));
            *task.as_mut().next.get_mut() = ptr::null_mut();
            task
        };

        Self {
            head: Some(task),
            tail: task,
            size: 1,
        }
    }
}

impl Default for TaskBatch {
    fn default() -> Self {
        Self::new()
    }
}

impl TaskBatch {
    /// Create an empty batch of tasks
    pub const fn new() -> Self {
        Self {
            head: None,
            tail: NonNull::dangling(),
            size: 0,
        }
    }

    /// Returns the amount of tasks currently present in the task batch
    #[inline]
    pub fn len(&self) -> usize {
        self.size
    }

    /// Enqueue a task or batch of tasks to the end of this task batch.
    ///
    /// # Safety:
    ///
    /// This is effectively an alias for [`push_back`] and requires the caller
    /// to uphold the same invariants.
    #[inline]
    pub unsafe fn push(&mut self, batch: impl Into<Self>) {
        self.push_back(batch)
    }

    /// Enqueue a task or batch of tasks to the end of this task batch.
    ///
    /// # Safety:
    ///
    /// Refer to the safety invariants listed at the documentation for
    /// [`TaskBatch`]
    pub unsafe fn push_back(&mut self, batch: impl Into<Self>) {
        let batch = batch.into();
        if let Some(batch_head) = batch.head {
            if self.head.is_some() {
                *self.tail.as_mut().next.get_mut() = batch_head.as_ptr();
                self.tail = batch.tail;
                self.size += batch.size;
            } else {
                *self = batch;
            }
        }
    }

    /// Enqueue a task or batch of tasks to the beginning of this task batch.
    ///
    /// # Safety:
    ///
    /// Refer to the safety invariants listed at the documentation for
    /// [`TaskBatch`]
    pub unsafe fn push_front(&mut self, batch: impl Into<Self>) {
        let mut batch = batch.into();
        if batch.head.is_some() {
            if let Some(head) = self.head {
                *batch.tail.as_mut().next.get_mut() = head.as_ptr();
                self.head = batch.head;
                self.size += batch.size;
            } else {
                *self = batch;
            }
        }
    }

    /// Dequeue a task from the beginning of this task batch.
    ///
    /// # Safety:
    ///
    /// This is effectively an alias for [`pop_front`] and requires the caller
    /// to uphold the same invariants.
    #[inline]
    pub unsafe fn pop(&mut self) -> Option<NonNull<Task>> {
        self.pop_front()
    }

    /// Dequeue a task from the beginning of this task batch.
    ///
    /// # Safety:
    ///
    /// Refer to the safety invariants listed at the documentation for
    /// [`TaskBatch`]
    pub unsafe fn pop_front(&mut self) -> Option<NonNull<Task>> {
        self.head.map(|mut task| {
            self.head = NonNull::new(*task.as_mut().next.get_mut());
            self.size -= 1;
            task
        })
    }
}
