mod buffer;
pub use buffer::{TaskBuffer, TaskListBuffer};

mod queue;
pub use queue::{TaskListQueue};

use core::{
    pin::Pin,
    ptr::{null_mut, NonNull},
};
use super::{Task, TaskPriority};

#[derive(Default, Debug)]
pub struct TaskList {
    head: Option<NonNull<Task>>,
    tail: Option<NonNull<Task>>,
    size: usize,
}

impl<'a> From<Pin<&'a mut Task>> for TaskList {
    #[inline]
    fn from(task: Pin<&'a mut Task<P>>) -> Self {
        let mut list = Self::new();
        list.push(task);
        list
    }
}

impl TaskList {
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
        task: Pin<&mut Task>,
    ) {
        self.size += 1;
        let task = NonNull::from(task.into_inner_unchecked());
        let (task_ptr, priority) = Task::decode(task);

        match priority {
            // Low and Normal priorities are pushed to the back
            TaskPriority::Low | TaskPriority::Normal => {
                *task_ptr.as_mut().next.get_mut() = ptr::null_mut();
                if let Some(tail) = self.tail {
                    *tail.as_mut().next.get_mut() = task_ptr.as_ptr();
                }
                if self.head.is_none() {
                    self.head = Some(task);
                }
                self.tail = Some(task);
            },
            // High and Critical priorities are pushed to the front
            TaskPriority::High | TaskPriority::Critical => {
                *task_ptr.as_mut().next.get_mut() = self
                    .head
                    .map(|p| p.as_ptr())
                    .unwrap_or(ptr::null_mut());
                if self.tail.is_none() {
                    self.tail = Some(task);
                }
                self.head = Some(task);
            },
        }
    }

    /// Pop a task from the front of the queue.
    pub unsafe fn pop(&mut self) -> Option<NonNull<Task>> {
        self.size -= 1;
        self.head.map(|task| {
            let (task_ptr, _) = Task::decode(task);
            let next_task = *task_ptr.as_mut().next.get_mut();
            self.head = NonNull::new(next_task);
            if self.head.is_none() {
                self.tail = None;
            }
            task
        })
    }
}




