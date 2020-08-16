use super::Thread;
use core::{
    iter::FromIterator,
    marker::{PhantomData, PhantomPinned},
    pin::Pin,
    ptr::NonNull,
    sync::atomic::AtomicUsize,
};

pub type Callback = extern "C" fn(Pin<&mut Task>, Pin<&Thread>);

#[repr(C)]
pub struct Task {
    pub(crate) next: AtomicUsize,
    pub(crate) callback: Callback,
    _pinned: PhantomPinned,
}

#[repr(C)]
pub struct Batch {
    pub(crate) head: Option<NonNull<Task>>,
    pub(crate) tail: NonNull<Task>,
}

impl Default for Batch {
    fn default() -> Self {
        Self::new()
    }
}

impl From<Pin<&mut Task>> for Batch {
    fn from(task: Pin<&mut Task>) -> Self {
        let task = unsafe { Pin::get_unchecked_mut(task) };
        *task.next.get_mut() = 0;
        let task = NonNull::from(task);

        Self {
            head: Some(task),
            tail: task,
        }
    }
}

impl Batch {
    pub const fn new() -> Self {
        Self {
            head: None,
            tail: NonNull::dangling(),
        }
    }

    pub fn is_empty(&self) -> bool {
        self.head.is_none()
    }

    pub fn push_front(&mut self, mut other: Self) {
        if let Some(_) = other.head {
            if let Some(head) = self.head {
                unsafe { *other.tail.as_mut().next.get_mut() = head.as_ptr() as usize };
                self.head = other.head;
            } else {
                *self = other;
            }
        }
    }

    pub fn push_back(&mut self, other: Self) {
        if let Some(other_head) = other.head {
            if let Some(_) = self.head {
                unsafe {
                    *self.tail.as_mut().next.get_mut() = other_head.as_ptr() as usize
                };
                self.tail = other.tail;
            } else {
                *self = other;
            }
        }
    }

    pub fn pop_front(&mut self) -> Option<NonNull<Task>> {
        self.head.map(|mut task| unsafe {
            let next = *task.as_mut().next.get_mut();
            self.head = NonNull::new(next as *mut Task);
            task
        })
    }

    pub fn iter(&self) -> BatchIter<'_> {
        BatchIter {
            task: self.head,
            _lifetime: PhantomData,
        }
    }

    pub fn drain(&mut self) -> BatchDrain<'_> {
        BatchDrain { batch: self }
    }
}

pub struct BatchIter<'a> {
    task: Option<NonNull<Task>>,
    _lifetime: PhantomData<&'a ()>,
}

impl<'a> Iterator for BatchIter<'a> {
    type Item = NonNull<Task>;

    fn next(&mut self) -> Option<Self::Item> {
        self.task.map(|mut task| {
            let next = unsafe { *task.as_mut().next.get_mut() };
            self.task = NonNull::new(next as *mut Task);
            task
        })
    }
}

pub struct BatchDrain<'a> {
    batch: &'a mut Batch,
}

impl<'a> Iterator for BatchDrain<'a> {
    type Item = NonNull<Task>;

    fn next(&mut self) -> Option<Self::Item> {
        self.batch.pop_front()
    }
}

impl<'a> FromIterator<Pin<&'a mut Task>> for Batch {
    fn from_iter<I: IntoIterator<Item = Pin<&'a mut Task>>>(iter: I) -> Self {
        iter.into_iter().fold(Self::new(), |mut batch, task| {
            batch.push_back(task.into());
            batch
        })
    }
}

impl IntoIterator for Batch {
    type Item = NonNull<Task>;
    type IntoIter = BatchIntoIter;

    fn into_iter(self) -> Self::IntoIter {
        BatchIntoIter { task: self.head }
    }
}

pub struct BatchIntoIter {
    task: Option<NonNull<Task>>,
}

impl Iterator for BatchIntoIter {
    type Item = NonNull<Task>;

    fn next(&mut self) -> Option<Self::Item> {
        self.task.map(|mut task| {
            let next = unsafe { *task.as_mut().next.get_mut() };
            self.task = NonNull::new(next as *mut Task);
            task
        })
    }
}
