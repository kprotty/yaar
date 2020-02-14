use super::{LinkedList, List, Priority, Task};
use crate::util::CachePadded;
use core::{
    cell::Cell,
    num::NonZeroUsize,
    convert::TryInto,
    fmt,
    mem::{size_of, transmute, MaybeUninit},
    ptr::NonNull,
    sync::atomic::{spin_loop_hint, AtomicUsize, Ordering},
};
use lock_api::{Mutex, RawMutex};

/// A FIFO, mutex protected, queue of [`Task`] pointers.
pub struct GlobalQueue<R: RawMutex> {
    size: AtomicUsize,
    list: Mutex<R, LinkedList>,
}

impl<R: RawMutex> Default for GlobalQueue<R> {
    fn default() -> Self {
        Self {
            size: AtomicUsize::new(0),
            list: Mutex::new(LinkedList::default()),
        }
    }
}

impl<R: RawMutex> fmt::Debug for GlobalQueue<R> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("GlobalQueue")
            .field("size", &self.len())
            .finish()
    }
}

impl<R: RawMutex> GlobalQueue<R> {
    /// Get an approximation of the global queue size.
    #[inline]
    pub fn len(&self) -> usize {
        self.size.load(Ordering::Relaxed)
    }

    /// Push a list of tasks all at once to the global queue.
    pub fn push(&self, list: List) {
        let List { front, back, size } = list;
        if size == 0 {
            return;
        }

        let mut global_queue = self.list.lock();
        global_queue.push_front(front);
        global_queue.push_back(back);
        self.size.store(self.len() + size, Ordering::Relaxed);
    }

    /// Dequeue a batch of tasks onto the given [`LocalQueue`],
    /// returning one of the task consumed.
    ///
    /// `max_local_queues` is used as a hint for distribution while
    /// `max_batch_size` limits the amount of tasks that can be dequeued.
    ///
    /// # Safety
    ///
    /// The pop operation assumes that the caller is the producer thread of
    /// the provided [`LocalQueue`]; The only thread that can call push(),
    /// pop() and pop_front() on it. Trying to pop() from the same
    /// LocalQueue on multiple threads may result in undefined behavior.
    pub unsafe fn pop(
        &self,
        local_queue: &LocalQueue,
        max_local_queues: NonZeroUsize,
        max_batch_size: NonZeroUsize,
    ) -> Option<NonNull<Task>> {
        // preemptively check size before trying to grab batch.
        let mut global_queue = self.list.lock();
        let size = self.size.load(Ordering::Relaxed);
        if size == 0 {
            return None;
        }

        // compute the amount of tasks to grab from the global queue
        let mut batch = ((size / max_local_queues.get()) + 1)
            .min(size)
            .min(max_batch_size.get())
            .min(LocalQueue::SIZE);
        self.size.store(size - batch, Ordering::Relaxed);

        // return the first task popped from the global queue
        let task = global_queue.pop();
        batch -= 1;

        // Push the remaining batch of tasks to the provided local queue.
        //
        // Safety:
        // * this thread should be the only producer thread for the local queue.
        // * the local queue provided must be empty if grabbing a batch of tasks.
        if batch != 0 {
            let mut tail = local_queue.load_tail_unsync();
            let mut head = local_queue.head().load(Ordering::Relaxed);
            assert_eq!(
                tail.wrapping_sub(head),
                0,
                "Should only steal large batch from global queue if local queue is empty"
            );

            for _ in 0..batch {
                let task = global_queue
                    .pop()
                    .map(|task_ptr| &*task_ptr.as_ptr())
                    .unwrap_or_else(|| core::hint::unreachable_unchecked());
                let priority = task.priority();
                let task = MaybeUninit::new(task as *const _);
                match priority {
                    Priority::Low | Priority::Normal => {
                        local_queue.tasks[(tail as usize) % LocalQueue::SIZE].set(task);
                        tail = tail.wrapping_add(1);
                    }
                    Priority::High | Priority::Critical => {
                        head = head.wrapping_sub(1);
                        local_queue.tasks[(head as usize) % LocalQueue::SIZE].set(task);
                    }
                }
            }

            let new_pos = LocalQueue::to_pos(head, tail);
            local_queue.pos().store(new_pos, Ordering::Release);
        }

        task
    }
}

use self::atomic_index::*;

#[cfg(target_pointer_width = "64")]
mod atomic_index {
    pub type PosIndex = u32;
    pub type AtomicIndex = core::sync::atomic::AtomicU32;
}

#[cfg(target_pointer_width = "32")]
mod atomic_index {
    pub type PosIndex = u16;
    pub type AtomicIndex = core::sync::atomic::AtomicU16;
}

/// A bound, single-producer, multi-consumer queue which supports stealing.
pub struct LocalQueue {
    pos: CachePadded<[AtomicIndex; 2]>,
    tasks: [Cell<MaybeUninit<*const Task>>; Self::SIZE],
}

/// Supports other threads calling [`LocalQueue::steal`].
unsafe impl Sync for LocalQueue {}

impl fmt::Debug for LocalQueue {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("LocalQueue")
            .field("size", &self.len())
            .finish()
    }
}

impl Default for LocalQueue {
    fn default() -> Self {
        Self {
            pos: CachePadded::new([AtomicIndex::new(0), AtomicIndex::new(0)]),
            // Safety: assume_init() is safe as all the array elements end up initialized.
            tasks: unsafe {
                let mut tasks = MaybeUninit::uninit();
                let ptr = tasks.as_mut_ptr() as *mut Cell<MaybeUninit<*const Task>>;
                for i in 0..Self::SIZE {
                    *ptr.add(i) = Cell::new(MaybeUninit::uninit());
                }
                tasks.assume_init()
            },
        }
    }
}

impl LocalQueue {
    // TODO: measure with the capacity.
    //       this is only the default in Golang and Tokio.
    const SIZE: usize = 256;
    const HEAD_POS: usize = 0;
    const TAIL_POS: usize = 1;

    /// Get a reference to the atomic head position of the ring buffer.
    pub(super) fn head(&self) -> &AtomicIndex {
        &self.pos[Self::HEAD_POS]
    }

    /// Get a reference to the atomic tail position of the ring buffer.
    pub(super) fn tail(&self) -> &AtomicIndex {
        &self.pos[Self::TAIL_POS]
    }

    /// Load the tail position of the ring buffer without synchronization.
    fn load_tail_unsync(&self) -> PosIndex {
        unsafe { *(self.tail() as *const _ as usize as *const PosIndex) }
    }

    /// Convert a head & tail value into a machine word to interact with
    /// [`pos()`].
    pub(super) fn to_pos(head: PosIndex, tail: PosIndex) -> usize {
        unsafe { transmute([head, tail]) }
    }

    /// Get an atomic reference to both the head and tail of the ring buffer.
    pub(super) fn pos(&self) -> &AtomicUsize {
        // Safety:
        // As long as the head & tail fit in the same atomic type,
        // they can both be interacted with atomically.
        assert_eq!(size_of::<AtomicUsize>(), size_of::<[AtomicIndex; 2]>());
        unsafe { &*(&self.pos as *const _ as *const _) }
    }

    /// Get an approximation of the local queue size.
    ///
    /// This is safe to be called by multiple threads.
    #[inline]
    pub fn len(&self) -> usize {
        let head = self.head().load(Ordering::Acquire);
        let tail = self.tail().load(Ordering::Acquire);
        tail.wrapping_sub(head) as usize
    }

    /// Push as task onto this queue, overflowing into the global queue if this
    /// queue is full.
    ///
    /// # Safety
    ///
    /// This should only be called by the producer thread of this local queue.
    pub unsafe fn push<R: RawMutex>(&self, task: &Task, global_queue: &GlobalQueue<R>) {
        match task.priority() {
            Priority::Low | Priority::Normal => self.push_back(task, global_queue),
            Priority::High | Priority::Critical => self.push_front(task, global_queue),
        }
    }

    /// Push as task to the end this queue, overflowing into the global queue if
    /// this queue is full.
    fn push_back<R: RawMutex>(&self, task: &Task, global_queue: &GlobalQueue<R>) {
        // Unsync load on tail as this thread is the only one that can update it.
        // Release load on head as not viewing `tasks` updates from other threads
        // since this should be the only thread that can write to `tasks`.
        let tail = self.load_tail_unsync();
        let mut head = self.head().load(Ordering::Release);

        loop {
            // Push the task to the tail if the queue isn't full.
            // Use a Release ordering on commit to make the task available
            // to `steal()`er threads which load the tail with Acquire.
            if (tail.wrapping_sub(head) as usize) < Self::SIZE {
                self.tasks[(tail as usize) % Self::SIZE].set(MaybeUninit::new(task));
                self.tail().store(tail.wrapping_add(1), Ordering::Release);
                return;
            }

            // Move a batch of tasks to the global queue if the local queue is full.
            match self.push_overflow(task, head, global_queue) {
                Ok(_) => return,
                Err(new_head) => head = new_head,
            }
        }
    }

    /// Push as task to the front this queue, overflowing into the global queue
    /// if this queue is full.
    fn push_front<R: RawMutex>(&self, task: &Task, global_queue: &GlobalQueue<R>) {
        // Unsync load on tail as this thread is the only one that can update it.
        // Release loads as not viewing `tasks` updates from other threads
        // since this should be the only thread that can write to `tasks`.
        let tail = self.load_tail_unsync();
        let mut head = self.head().load(Ordering::Release);

        loop {
            // Push the task to the head if the queue isn't full.
            // Use a Release ordering on commit to make the task available
            // to `steal()`er threads which load the head with Acquire.
            if (tail.wrapping_sub(head) as usize) < Self::SIZE {
                let new_head = head.wrapping_sub(1);
                self.tasks[(new_head as usize) % Self::SIZE].set(MaybeUninit::new(task));
                match self.head().compare_exchange_weak(
                    head,
                    new_head,
                    Ordering::Release,
                    Ordering::Release,
                ) {
                    Ok(_) => return,
                    Err(new_head) => head = new_head,
                }
                continue;
            }

            // Move a batch of tasks to the global queue if the local queue is full.
            match self.push_overflow(task, head, global_queue) {
                Ok(_) => return,
                Err(new_head) => head = new_head,
            }
        }
    }

    /// Move a batch of tasks from our local queue into the
    /// global queue to amortize locking the global queue on `push()`.
    ///
    /// Returns `Ok(())` on tasks successfully migrated and `Err(PosIndex)`
    /// if it failed to do so with the newly observed head.
    fn push_overflow<R: RawMutex>(
        &self,
        task: &Task,
        head: PosIndex,
        global_queue: &GlobalQueue<R>,
    ) -> Result<(), PosIndex> {
        // Try to grab half of our tasks to move to the global queue.
        // Uses Release ordering as no other thread should be able to
        // write to our tasks for us to observe since we're the only producer.
        let batch = Self::SIZE / 2;
        if let Err(new_head) = self.head().compare_exchange_weak(
            head,
            head.wrapping_add(batch.try_into().unwrap()),
            Ordering::Release,
            Ordering::Release,
        ) {
            return Err(new_head);
        }

        // Bundle up the batch of tasks into a linked list
        let mut batch_list = List::default();
        batch_list.push(task);
        for i in 0..batch {
            let task = self.tasks[(head as usize).wrapping_add(i) % Self::SIZE].get();
            batch_list.push(unsafe { task.assume_init() });
        }

        // Then submit the list in one go to keep the critical section short.
        global_queue.push(batch_list);
        Ok(())
    }

    /// Attempt to dequeue a task from the back of the queue.
    ///
    /// # Safety:
    ///
    /// This should only be called by the producer thread of this local queue.
    pub unsafe fn pop(&self) -> Option<NonNull<Task>> {
        // Unsync load on tail as this is the only thread that can update it.
        // Release loads as not viewing `tasks` updates from other threads.
        let tail = self.load_tail_unsync();
        let mut head = self.head().load(Ordering::Release);

        loop {
            // Our queue is empty.
            if tail.wrapping_sub(head) == 0 {
                return None;
            }

            // Read the head task in the queue, using a Release
            // ordering since not making any writes visible to other threads.
            let task = self.tasks[(head as usize) % Self::SIZE].get();
            match self.head().compare_exchange_weak(
                head,
                head.wrapping_add(1),
                Ordering::Release,
                Ordering::Release,
            ) {
                Ok(_) => return NonNull::new(task.assume_init() as *mut _),
                Err(new_head) => head = new_head,
            }
        }
    }

    /// Attempt to dequeue a task from the front of the queue.
    ///
    /// # Safety:
    ///
    /// This should only be called by the producer thread of this local queue.
    pub unsafe fn pop_front(&self) -> Option<NonNull<Task>> {
        // Unsync load on tail as this is the only thread that can update it.
        // Release loads as not viewing `tasks` updates from other threads.
        let tail = self.load_tail_unsync();
        let mut head = self.head().load(Ordering::Release);

        loop {
            // Our queue is empty.
            if tail.wrapping_sub(head) == 0 {
                return None;
            }

            // Read the tail task in the queue in order to return it.
            //
            // When updating the tail, the head could change from a
            // `steal()`ing consumer thread so the head & tail need to
            // be modified at the same time. Uses a Release ordering since
            // other threads nor our own is make any writes visible.
            let new_tail = tail.wrapping_sub(1);
            let task = self.tasks[(new_tail as usize) % Self::SIZE].get();
            match self.pos().compare_exchange_weak(
                Self::to_pos(head, tail),
                Self::to_pos(head, new_tail),
                Ordering::Release,
                Ordering::Release,
            ) {
                Ok(_) => return NonNull::new(task.assume_init() as *mut _),
                Err(new_pos) => head = transmute::<_, [PosIndex; 2]>(new_pos)[Self::HEAD_POS],
            }
        }
    }

    /// Attempt to steal a batch of tasks from `other`s queue into our own,
    /// returning one of the tasks stolen if any.
    ///
    /// This is safe to be called from the consumer threads while the producer
    /// thread is running and panics if the current LocalQueue is empty.
    pub fn steal(&self, other: &Self) -> Option<NonNull<Task>> {
        // Unsync load on tail as this is the only thread that can update it.
        // Release ordering since not observing any changes from other threads.
        let tail = self.load_tail_unsync();
        let head = self.head().load(Ordering::Release);
        assert_eq!(
            tail.wrapping_sub(head),
            0,
            "Should only steal if our queue is empty"
        );

        loop {
            // Acquire loads to make the task writes from the 'other' producer thread
            // visible.
            let other_head = other.head().load(Ordering::Acquire);
            let other_tail = other.tail().load(Ordering::Acquire);

            // Bail if theres nothing to steal.
            let size = other_tail.wrapping_sub(other_head);
            if size == 0 {
                return None;
            }

            // Copy a batch of tasks from other's queue into ours
            let mut batch = size - (size / 2);
            for i in 0..batch {
                let task = other.tasks[(other_head.wrapping_add(i) as usize) % Self::SIZE].get();
                self.tasks[(tail.wrapping_add(i) as usize) % Self::SIZE].set(task);
            }

            // Try to commit the steal. Release ordering is ok since
            // no writes to communicate to other's producer thread.
            if other
                .head()
                .compare_exchange_weak(
                    other_head,
                    other_head.wrapping_add(batch),
                    Ordering::Release,
                    Ordering::Release,
                )
                .is_err()
            {
                spin_loop_hint();
                continue;
            }

            // Make the tasks we stole available excluding the last.
            batch -= 1;
            let new_tail = tail.wrapping_add(batch);
            if batch != 0 {
                self.tail().store(new_tail, Ordering::Release);
            }

            // Use the last task as the return value.
            let task = self.tasks[(new_tail as usize) % Self::SIZE].get();
            return NonNull::new(unsafe { task.assume_init() as *mut _ });
        }
    }
}
