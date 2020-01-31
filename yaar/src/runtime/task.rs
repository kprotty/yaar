use super::with_executor;
use core::{
    cell::Cell,
    convert::TryInto,
    fmt,
    mem::{align_of, size_of, transmute, MaybeUninit},
    ptr::{null_mut, NonNull},
    sync::atomic::{spin_loop_hint, AtomicUsize, Ordering},
};
use lock_api::{Mutex, RawMutex};

const PTR_TAG_MASK: usize = 0b11;

pub enum Priority {
    Low = 0b00,
    Normal = 0b01,
    High = 0b10,
    Critical = 0b11,
}

pub struct Task {
    next: Cell<usize>,
    state: AtomicUsize,
}

unsafe impl Sync for Task {}

impl Task {
    #[inline]
    pub fn new(priority: Priority, resume: unsafe fn(*const Self)) -> Self {
        assert!(align_of::<Self>() > PTR_TAG_MASK);
        Self {
            next: Cell::new(priority as usize),
            state: AtomicUsize::new(resume as usize),
        }
    }

    pub(super) fn next(&self) -> Option<NonNull<Self>> {
        NonNull::new((self.next.get() & !PTR_TAG_MASK) as *mut Self)
    }

    pub(super) fn set_next(&self, ptr: Option<NonNull<Self>>) {
        let ptr = ptr.map(|p| p.as_ptr()).unwrap_or(null_mut());
        self.next
            .set((self.next.get() & PTR_TAG_MASK) | (ptr as usize));
    }

    #[inline]
    pub fn priority(&self) -> Priority {
        match self.next.get() & PTR_TAG_MASK {
            0 => Priority::Low,
            1 => Priority::Normal,
            2 => Priority::High,
            3 => Priority::Critical,
            _ => unreachable!(),
        }
    }

    #[inline]
    pub unsafe fn resume(&self) {
        let state = self.state.load(Ordering::Relaxed);
        let resume_fn: unsafe fn(*const Self) = transmute(state & !PTR_TAG_MASK);
        self.state.store(resume_fn as usize, Ordering::Relaxed);
        resume_fn(self)
    }

    #[inline]
    pub fn schedule(&self) {
        let resume_fn = self.state.load(Ordering::Relaxed) & !PTR_TAG_MASK;
        if self.state.swap(resume_fn | 1, Ordering::Relaxed) == resume_fn {
            with_executor(|e| e.schedule(self));
        }
    }
}

#[derive(Default)]
struct LinkedList {
    head: Option<NonNull<Task>>,
    tail: Option<NonNull<Task>>,
}

impl LinkedList {
    pub fn push_front(&mut self, list: Self) {
        if let Some(mut tail) = self.tail {
            unsafe { tail.as_mut().set_next(list.head) }
        }
        if self.head.is_none() {
            self.head = list.head;
        }
        self.tail = list.tail;
    }

    pub fn push_back(&mut self, list: Self) {
        if let Some(tail) = list.tail {
            unsafe { tail.as_ref().set_next(self.head) }
        }
        if self.tail.is_none() {
            self.tail = list.tail;
        }
        self.head = list.head;
    }

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

#[derive(Default)]
pub struct List {
    front: LinkedList,
    back: LinkedList,
    size: usize,
}

impl List {
    #[inline]
    pub fn len(&self) -> usize {
        self.size
    }

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
}

#[derive(Default)]
pub struct GlobalQueue<R: RawMutex> {
    size: AtomicUsize,
    list: Mutex<R, LinkedList>,
}

impl<R: RawMutex> fmt::Debug for GlobalQueue<R> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("GlobalQueue")
            .field("size", &self.len())
            .finish()
    }
}

impl<R: RawMutex> GlobalQueue<R> {
    #[inline]
    pub fn len(&self) -> usize {
        self.size.load(Ordering::Relaxed)
    }

    pub fn push(&self, list: List) {
        let List { front, back, size } = list;
        if size == 0 {
            return;
        }

        let mut queue = self.list.lock();
        queue.push_front(front);
        queue.push_back(back);
        self.size.store(self.len() + size, Ordering::Relaxed);
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
    pos: [AtomicIndex; 2],
    tasks: [Cell<MaybeUninit<*const Task>>; Self::SIZE],
}

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
            pos: [AtomicIndex::new(0), AtomicIndex::new(0)],
            // Safety: assume_init() safe as all the array elements end up initialized.
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

    /// Convert a head & tail value into a machine word to interact with
    /// [`pos()`].
    pub(super) fn to_pos(head: PosIndex, tail: PosIndex) -> usize {
        unsafe { transmute([head, tail]) }
    }

    /// Get an atomic reference to both the head and tail of the ring buffer.
    pub(super) fn pos(&self) -> &AtomicUsize {
        // Safety: as long as the head & tail fit in the same atomic type,
        // they can both be interacted with atomically.
        assert_eq!(size_of::<AtomicUsize>(), size_of::<[AtomicIndex; 2]>());
        unsafe { &*(&self.pos as *const _ as *const _) }
    }

    /// Get an approximation of the local queue size.
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
    pub unsafe fn push<R: RawMutex>(&self, task: *const Task, global_queue: &GlobalQueue<R>) {
        let task = &*task;
        match task.priority() {
            Priority::Low | Priority::Normal => self.push_back(task, global_queue),
            Priority::High | Priority::Critical => self.push_front(task, global_queue),
        }
    }

    /// Push as task to the end this queue, overflowing into the global queue if
    /// this queue is full.
    fn push_back<R: RawMutex>(&self, task: &Task, global_queue: &GlobalQueue<R>) {
        // Relaxed loads as not viewing `tasks` updates from other threads
        // since this should be the only thread that can write to `tasks`.
        let tail = self.tail().load(Ordering::Relaxed);
        let mut head = self.head().load(Ordering::Relaxed);

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
        // Relaxed loads as not viewing `tasks` updates from other threads
        // since this should be the only thread that can write to `tasks`.
        let tail = self.tail().load(Ordering::Relaxed);
        let mut head = self.head().load(Ordering::Relaxed);

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
                    Ordering::Relaxed,
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
        // Uses Relaxed ordering as no other thread should be able to
        // write to our tasks for us to observe since we're the only producer.
        let batch = Self::SIZE / 2;
        if let Err(new_head) = self.head().compare_exchange_weak(
            head,
            head.wrapping_add(batch.try_into().unwrap()),
            Ordering::Relaxed,
            Ordering::Relaxed,
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
        // Relaxed loads as not viewing `tasks` updates from other threads.
        let tail = self.tail().load(Ordering::Relaxed);
        let mut head = self.head().load(Ordering::Relaxed);

        loop {
            // Our queue is empty.
            if tail.wrapping_sub(head) == 0 {
                return None;
            }

            // Read the head task in the queue, using a Relaxed
            // ordering since not making any writes visible to other threads.
            let task = self.tasks[(head as usize) % Self::SIZE].get();
            match self.head().compare_exchange_weak(
                head,
                head.wrapping_add(1),
                Ordering::Relaxed,
                Ordering::Relaxed,
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
        // Relaxed loads as not viewing `tasks` updates from other threads.
        let tail = self.tail().load(Ordering::Relaxed);
        let mut head = self.head().load(Ordering::Relaxed);

        loop {
            // Our queue is empty.
            if tail.wrapping_sub(head) == 0 {
                return None;
            }

            // Read the tail task in the queue in order to return it.
            //
            // When updating the tail, the head could change from a
            // `steal()`ing consumer thread so the head & tail need to
            // be modified at the same time. Uses a Relaxed ordering since
            // other threads nor our own is make any writes visible.
            let new_tail = tail.wrapping_sub(1);
            let task = self.tasks[(new_tail as usize) % Self::SIZE].get();
            match self.pos().compare_exchange_weak(
                Self::to_pos(head, tail),
                Self::to_pos(head, new_tail),
                Ordering::Relaxed,
                Ordering::Relaxed,
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
        // Relaxed ordering since not observing any changes from other threads.
        let tail = self.tail().load(Ordering::Relaxed);
        let head = self.head().load(Ordering::Relaxed);
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

            // Try to commit the steal. Relaxed ordering is ok since
            // no writes to communicate to other's producer thread.
            if other
                .head()
                .compare_exchange_weak(
                    other_head,
                    other_head.wrapping_add(batch),
                    Ordering::Relaxed,
                    Ordering::Relaxed,
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
