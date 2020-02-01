use super::{Priority, Task};
use core::ptr::NonNull;

/// Intrusive linked list of tasks.
#[derive(Default)]
pub(crate) struct LinkedList {
    head: Option<NonNull<Task>>,
    tail: Option<NonNull<Task>>,
}

impl LinkedList {
    /// Push a linked list of tasks to the front of this linked list
    pub fn push_front(&mut self, list: Self) {
        if let Some(mut tail) = self.tail {
            unsafe { tail.as_mut().set_next(list.head) }
        }
        if self.head.is_none() {
            self.head = list.head;
        }
        self.tail = list.tail;
    }

    /// Push a linked list of tasks to the back of this linked list
    pub fn push_back(&mut self, list: Self) {
        if let Some(tail) = list.tail {
            unsafe { tail.as_ref().set_next(self.head) }
        }
        if self.tail.is_none() {
            self.tail = list.tail;
        }
        self.head = list.head;
    }

    /// Pop a task from the front of this linked list
    pub fn pop(&mut self) -> Option<NonNull<Task>> {
        self.head.map(|task| {
            self.head = unsafe { task.as_ref().next() };
            if self.head.is_none() {
                self.tail = None;
            }
            task
        })
    }
}

/// An intrusive list of Tasks which does prioritization internally.
#[derive(Default)]
pub struct List {
    pub(crate) front: LinkedList,
    pub(crate) back: LinkedList,
    pub(crate) size: usize,
}

impl List {
    /// Returns the number of tasks in the list
    #[inline]
    pub fn len(&self) -> usize {
        self.size
    }

    /// Push a task pointer to the linked list using the priority for sorting.
    /// This takes a pointer instead of a reference since the task should live
    /// at least until it is popped from the list or the list is consumed.
    pub fn push(&mut self, task: *const Task) {
        let task = unsafe { &*task };
        task.set_next(None);
        let list = LinkedList {
            head: NonNull::new(task as *const _ as *mut _),
            tail: NonNull::new(task as *const _ as *mut _),
        };

        self.size += 1;
        match task.priority() {
            Priority::Low | Priority::Normal => self.back.push_back(list),
            Priority::High | Priority::Critical => self.front.push_back(list),
        }
    }

    /// Pop a task from the list with the highest internal priority.
    pub fn pop(&mut self) -> Option<NonNull<Task>> {
        self.front.pop().or_else(|| self.back.pop()).map(|task| {
            self.size -= 1;
            task
        })
    }
}
