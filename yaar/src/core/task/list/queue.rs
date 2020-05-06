use super::{
    TaskList,
    TaskBuffer,
    TaskListBuffer,
    super::{Task, TaskPriority},
};
use core::{
    pin::Pin,
    ptr::{self, NonNull},
    cell::UnsafeCell,
    marker::PhantomPinned,
    num::NonZeroUsize,
    sync::atomic::{Ordering, AtomicUsize, AtomicPtr},
};
use yaar_lock::utils::CachePadded;

pub struct TaskListQueue {
    head: CachePadded<AtomicPtr<Task>>,
    lock: CachePadded<AtomicUsize>,
    tail: UnsafeCell<NonNull<Task>>,
    stub: AtomicPtr<Task>,
    _pinned: PhantomPinned,
}

impl TaskListQueue {
    pub unsafe fn init(self: Pin<&mut Self>) {
        let mut_self = self.into_inner_unchecked();
        let stub_ptr = &mut_self.stub as *const _ as *mut Task;
        *mut_self = Self {
            head: CachePadded::new(AtomicPtr::new(stub_ptr)),
            lock: CachePadded::new(AtomicUsize::new(0)),
            tail: UnsafeCell::new(NonNull::new_unchecked(stub_ptr)),
            stub: AtomicPtr::new(ptr::null_mut()),
        };
    }

    pub unsafe fn push(self: Pin<&Self>, list: TaskList) {

    }

    pub unsafe fn try_steal(
        self: Pin<&Self>,
        list: &TaskListBuffer,
    ) -> Option<NonNull<Task>> {
        self.try_with_tail(|tail| {

        })
    }

    pub unsafe fn try_steal_into(
        self: Pin<&Self>,
        buffer_start: usize,
        buffer_size: NonZeroUsize,
        buffer: &TaskBuffer,
    ) -> Option<NonZeroUsize> {
        self.try_with_tail(|tail| {

        })
    }

    fn try_with_tail<T>(
        &self,
        f: impl FnOnce(&mut NonNull<Task>) -> Option<T>,
    ) -> Option<T> {
        // TODO: if nightly & x86/x86_64, use `lock bts` instead.
        let acquired = {
            self.lock.compare_and_swap(0, 1, Ordering::Acquire) == 0
        };

        if acquired {
            let result = f(unsafe { &mut *self.tail.get() });
            self.lock.store(0, Ordering::Release);
            result
        } else {
            None
        }
    }
}