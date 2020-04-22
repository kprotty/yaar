use super::task::{Task, Priority};
use lock_api::{Mutex, RawMutex};
use yaar_lock::utils::CachePadded;
use core::{
    cell::Cell,
    ptr::{write, NonNull},
    mem::MaybeUninit,
    sync::atomic::{spin_loop_hint, Ordering, AtomicUsize},
};

#[derive(Default, Debug)]
pub struct ListQueue {
    head: Cell<Option<NonNull<Task>>>,
    tail: Cell<Option<NonNull<Task>>>,
    size: usize,
}

impl From<NonNull<Task>> for ListQueue {
    fn from(task: NonNull<Task>) -> Self {
        Self {
            head: Cell::new(Some(task)),
            tail: Cell::new(Some(task)),
            size: 1,
        }
    }
}

impl ListQueue {
    pub fn push(&mut self, task: NonNull<Task>) {
        unimplemented!()
    }

    pub fn pop(&mut self) -> Option<NonNull<Task>> {
        unimplemented!()
    }
}

#[derive(Default, Debug)]
pub struct GlobalQueue<R: RawMutex> {
    queue: Mutex<R, ListQueue>,
}

impl<R: RawMutex> GlobalQueue<R> {
    pub fn push(&self, queue: ListQueue) {
        unimplemented!()
    }

    pub unsafe fn pop(
        &self,
        local_queue: &LocalQueue,
        distribution: usize,
        max_pop: usize,
    ) -> Option<NonNull<Task>> {
        unimplemented!()
    }
}

pub struct LocalQueue {
    head: CachePadded<AtomicUsize>,
    tail: CachePadded<AtomicUsize>,
    queue: [Cell<MaybeUninit<NonNull<Task>>>; Self::SIZE],
}

unsafe impl Sync for LocalQueue {}

impl Default for LocalQueue {
    fn default() -> Self {
        Self {
            head: CachePadded::new(AtomicUsize::new(0)),
            tail: CachePadded::new(AtomicUsize::new(0)),
            queue: unsafe {
                let mut queue: MaybeUninit<[_; Self::SIZE]> = MaybeUninit::uninit();
                let queue_ptr = queue.as_mut_ptr() as *mut Cell<MaybeUninit<NonNull<Task>>>;
                for i in 0..Self::SIZE {
                    write(queue_ptr.add(i), Cell::new(MaybeUninit::uninit()));
                }
                queue.assume_init()
            },
        }
    }
}

impl LocalQueue {
    const SIZE: usize = 256;
}