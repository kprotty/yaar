use super::get_executor_ref;
use core::{
    convert::{AsMut, AsRef, TryInto},
    future::Future,
    hint::unreachable_unchecked,
    marker::PhantomPinned,
    mem::{self, align_of, MaybeUninit},
    num::{NonZeroU64, NonZeroUsize},
    pin::Pin,
    ptr::NonNull,
    sync::atomic::{spin_loop_hint, AtomicUsize, Ordering},
    task::{Context, Poll, RawWaker, RawWakerVTable, Waker},
};
use yaar_lock::sync::{RawMutex, ThreadParker};

macro_rules! field_parent_ptr {
    ($type:ty, $field:ident, $ptr:expr) => {{
        // TODO: A non-UB way of offsetof() on stable?
        let stub = MaybeUninit::<$type>::zeroed();
        let base = stub.as_ptr() as usize;
        let field = &(*stub.as_ptr()).$field as *const _ as usize;
        &mut *((($ptr) as usize - (field - base)) as *mut $type)
    }};
}

/// Importance level of a task used to influence its scheduling delay.
///
/// The scheduler will make an effort to resume `High` priority tasks
/// before `Normal` priority tasks, and so on.
#[derive(Copy, Clone, PartialEq)]
pub enum TaskPriority {
    Low = 0,
    Normal = 1,
    High = 2,
}

impl TaskPriority {
    pub const MASK: usize = 3;
}

/// A unit of execution to the runtime scheduler.
///
/// Code that wishes to run concurrently should express itself
/// by units of tasks. This is separate from [`Future`]s and it
/// allows finer-grain and dynamic approaches to code execution.
///
/// For tasks integrated with [`Future`]s, see [`FutureTask`].
pub struct Task {
    next: usize,
    resume: unsafe fn(*mut Self),
}

unsafe impl Send for Task {}

impl Task {
    /// Create a new task using the provided priority
    /// and a function to be called when resumed.
    ///
    /// The resume function uses a pointer to the current
    /// task in order to continue execution. It can be
    /// invoked manually through [`resume`].
    ///
    /// [`resume`]: struct.Task.html#method.resume
    pub fn new(priority: TaskPriority, resume: unsafe fn(*mut Self)) -> Self {
        debug_assert!(align_of::<Self>() > TaskPriority::MASK);
        Self {
            next: priority as usize,
            resume,
        }
    }

    /// Tasks also function as intrusive linked list nodes.
    ///
    /// Set the next field of the task to another task pointer.
    pub(super) fn set_next(&mut self, task: Option<NonNull<Self>>) {
        let task = task.map(|ptr| ptr.as_ptr() as usize).unwrap_or(0);
        self.next = (self.next & TaskPriority::MASK) | task;
    }

    /// Tasks also function as intrusive linked list nodes.
    ///
    /// Set the next field of the task to another task pointer.
    pub(super) fn next(&self) -> Option<NonNull<Self>> {
        NonNull::new((self.next & !TaskPriority::MASK) as *mut Self)
    }

    /// Modify the [`TaskPriority`] of the current task.
    ///
    /// It is undefined behavior to update the priority before the
    /// task has been resumed after being [`schedule`]'d.
    ///
    /// [`schedule`]: struct.Task.html#method.schedule
    pub fn set_priority(&mut self, priority: TaskPriority) {
        self.next = (self.next & !TaskPriority::MASK) | (priority as usize);
    }

    /// Get the [`TaskPriority`] of the task.
    pub fn priority(&self) -> TaskPriority {
        match self.next & TaskPriority::MASK {
            0 => TaskPriority::Low,
            1 => TaskPriority::Normal,
            2 => TaskPriority::High,
            _ => unsafe {
                debug_assert!(false, "invalid TaskPriority");
                unreachable_unchecked()
            },
        }
    }

    /// Invoke the resume function of the task manually.
    /// This may be useful for implementing custom scheduling.
    ///
    /// # Safety
    ///
    /// The caller should ensure that the task isn't already scheduled
    /// onto the executor and that the resume function used to create
    /// the task is inheritly safe.
    #[inline]
    pub unsafe fn resume(&mut self) {
        (self.resume)(self)
    }

    /// Schedule the given task onto the executor in order to eventually
    /// call it's resume function for execution. If there is no current
    /// running executor, it does nothing.
    ///
    /// # Safety
    ///
    /// As this interacts with executor internals directly, the caller
    /// should uphold that the task's resume function is both safe to
    /// call and that the task has not been scheduled more than once
    /// before it's resume was called. If this were the case, the executor
    /// could call the resume function in paralell causing undefined behaviour.
    pub unsafe fn schedule(&mut self) {
        if let Some(e) = get_executor_ref() {
            (e.schedule)(e.ptr, self)
        }
    }

    /// Poll a given future using the task as a [`Waker`] implementation which
    /// calls [`schedule`] on the task once woken through `wake()` or `wake_by_ref()`.
    ///
    /// # Safety
    ///
    /// The caller should ensure the same rules as [`schedule`] with regards
    /// to the safety of poll(). The task should also ideally have a method
    /// of continuing the future as well but is not required.
    ///
    /// [`schedule`]: struct.Task.html#method.schedule
    pub unsafe fn poll<F: Future>(&mut self, future: Pin<&mut F>) -> Poll<F::Output> {
        const VTABLE: RawWakerVTable = RawWakerVTable::new(
            |ptr| RawWaker::new(ptr, &VTABLE),
            |ptr| unsafe { (&mut *(ptr as *mut Task)).schedule() },
            |ptr| unsafe { (&mut *(ptr as *mut Task)).schedule() },
            |_| {},
        );

        let raw_waker = RawWaker::new(self as *const Self as *const (), &VTABLE);
        let waker = Waker::from_raw(raw_waker);
        let mut ctx = Context::from_waker(&waker);
        future.poll(&mut ctx)
    }
}

/// A [`Future`] associated with a [`Task`].
///
/// This is the primary method used to express concurrency
/// for futures using the runtime's task system.
pub struct FutureTask<F> {
    pinned: PhantomPinned,
    future: F,
    task: Task,
}

impl<F> AsRef<Task> for FutureTask<F> {
    fn as_ref(&self) -> &Task {
        &self.task
    }
}

impl<F> AsMut<Task> for FutureTask<F> {
    fn as_mut(&mut self) -> &mut Task {
        &mut self.task
    }
}

impl<F: Future> Future for FutureTask<F> {
    type Output = F::Output;

    fn poll(self: Pin<&mut Self>, ctx: &mut Context<'_>) -> Poll<Self::Output> {
        // this is safe as the Future should be !Unpin from the PhantomPinned
        unsafe { self.map_unchecked_mut(|this| &mut this.future).poll(ctx) }
    }
}

impl<F: Future> FutureTask<F> {
    /// Create a new task using the priority and associated using the given future.
    pub fn new(priority: TaskPriority, future: F) -> Self {
        Self {
            pinned: PhantomPinned,
            future,
            task: Task::new(priority, |task_ptr| unsafe {
                // To resume: using a ptr to task, find Self via offsetof() and poll() again.
                let this = field_parent_ptr!(Self, task, task_ptr);
                let _ = this.task.poll(Pin::new_unchecked(&mut this.future));
            }),
        }
    }

    /// Associate a future with an already existing task.
    ///
    /// # Safety
    ///
    /// It is expected that when polling, the task's resume
    /// function should have a method of re-polling the future.
    /// If not, the future could possibly dead-lock and never resolve.
    pub unsafe fn from_task(task: Task, future: F) -> Self {
        Self {
            pinned: PhantomPinned,
            future,
            task,
        }
    }
}

/// A [`Future`] and [`Task`] association similar to [`FutureTask`]
/// but caches the output of the future's poll internally.
pub struct CachedFutureTask<F: Future> {
    pinned: PhantomPinned,
    output: Option<F::Output>,
    future: F,
    task: Task,
}

impl<F: Future> AsRef<Task> for CachedFutureTask<F> {
    fn as_ref(&self) -> &Task {
        &self.task
    }
}

impl<F: Future> AsMut<Task> for CachedFutureTask<F> {
    fn as_mut(&mut self) -> &mut Task {
        &mut self.task
    }
}

impl<F: Future> Future for CachedFutureTask<F> {
    type Output = ();

    fn poll(self: Pin<&mut Self>, _ctx: &mut Context<'_>) -> Poll<Self::Output> {
        // return ready if the output has already been cached/consumed.
        if self.output.is_some() {
            return Poll::Ready(());
        }

        // see `FutureTask` above
        unsafe {
            let this = self.get_unchecked_mut();
            let pinned = Pin::new_unchecked(&mut this.future);
            match this.task.poll(pinned) {
                Poll::Pending => Poll::Pending,
                Poll::Ready(output) => {
                    this.output = Some(output);
                    Poll::Ready(())
                }
            }
        }
    }
}

impl<F: Future> CachedFutureTask<F> {
    /// Create a new task + future association with an empty cached output.
    pub fn new(priority: TaskPriority, future: F) -> Self {
        Self {
            pinned: PhantomPinned,
            output: None,
            future,
            task: Task::new(priority, |task_ptr| unsafe {
                let this = field_parent_ptr!(Self, task, task_ptr);
                let _ = (&mut *task_ptr).poll(Pin::new_unchecked(this));
            }),
        }
    }

    /// Check the state of the output. Also used as a method of
    /// checking for the future's completion.
    pub fn peek(&self) -> Option<&F::Output> {
        self.output.as_ref()
    }

    /// Consume the cached output, returning it if set by the completed future.
    pub fn into_inner(self) -> Option<F::Output> {
        self.output
    }
}

pub fn yield_now(priority: TaskPriority) -> impl Future<Output = ()> {
    struct YieldFuture {
        pinned: PhantomPinned,
        waker: Option<Waker>,
        yielded: bool,
        task: Task,
    }

    impl Future for YieldFuture {
        type Output = ();

        fn poll(self: Pin<&mut Self>, ctx: &mut Context<'_>) -> Poll<Self::Output> {
            if self.yielded {
                return Poll::Ready(());
            }

            // it is safe to get_unchecked_mut()/schedule() since
            // the future is !Unpin and wont move.
            unsafe {
                let this = self.get_unchecked_mut();
                this.waker = Some(ctx.waker().clone());
                this.yielded = true;
                this.task.schedule();
                Poll::Pending
            }
        }
    }

    YieldFuture {
        pinned: PhantomPinned,
        waker: None,
        yielded: false,
        task: Task::new(priority, |task_ptr| unsafe {
            let this = field_parent_ptr!(YieldFuture, task, task_ptr);
            mem::replace(&mut this.waker, None).unwrap().wake();
        }),
    }
}

/// Run the given function without blocking the executor.
///
/// `max_block_ms` can be used as a hint in milliseconds for how long the function
/// is allowed to block before the runtime possibly spawns a new thread to handle
/// other tasks. Providing a value of `None` is akin to having a timeout of 0ms.
///
/// A future with the result is returned as it needs to wait to start running under
/// the runtime's executor again if the `max_block_ms` timeout ran out.
pub fn run_blocking<T>(
    max_block_ms: Option<NonZeroU64>,
    f: impl FnOnce() -> T,
) -> impl Future<Output = T> {
    // TODO
    unreachable!();

    struct Stub<T>(T);
    impl<T> Future for Stub<T> {
        type Output = T;
        fn poll(self: Pin<&mut Self>, ctx: &mut Context<'_>) -> Poll<Self::Output> {
            Poll::Pending
        }
    }
    Stub(f())
}

/// Am intrusive linked list of [`Task`]s
#[derive(Default)]
struct LinkedList {
    head: Option<NonNull<Task>>,
    tail: Option<NonNull<Task>>,
}

impl LinkedList {
    /// Pop one task from the front of the list.
    pub fn pop<'a>(&mut self) -> Option<&'a mut Task> {
        self.head.map(|task| {
            let task = unsafe { &mut *task.as_ptr() };
            self.head = task.next();
            if self.head.is_none() {
                self.tail = None;
            }
            task
        })
    }

    /// Push an entire list at once to the back of this list
    pub fn push(&mut self, list: Self) {
        if let Some(mut tail) = self.tail {
            unsafe { tail.as_mut().set_next(list.head) }
        }
        if self.head.is_none() {
            self.head = list.head;
        }
        self.tail = list.tail;
    }

    /// Push an entire list at once to the front of this list
    pub fn push_front(&mut self, list: Self) {
        if let Some(mut tail) = list.tail {
            unsafe { tail.as_mut().set_next(self.head) }
        }
        if self.tail.is_none() {
            self.tail = list.tail;
        }
        self.head = list.head;
    }
}

// A prioritized, intrusive linked list of [`Task`]s.
#[derive(Default)]
pub(super) struct TaskList {
    front: LinkedList,
    back: LinkedList,
    size: usize,
}

impl TaskList {
    /// Get the size of the task list
    #[inline]
    pub fn len(&self) -> usize {
        self.size
    }

    /// Push the task to the internal list, ordering it based on it's priority
    pub fn push(&mut self, task: &mut Task) {
        task.set_next(None);
        let list = LinkedList {
            head: NonNull::new(task),
            tail: NonNull::new(task),
        };

        self.size += 1;
        match task.priority() {
            TaskPriority::Low | TaskPriority::Normal => self.back.push(list),
            TaskPriority::High => self.front.push(list),
        }
    }
}

#[derive(Default)]
pub(super) struct TaskInjector {
    list: LinkedList,
    size: usize,
}

impl TaskInjector {
    pub fn push(&mut self, list: TaskList) {
        let TaskList { front, back, size } = list;
        if size != 0 {
            self.list.push_front(front);
            self.list.push(back);
            self.size += size;
        }
    }

    pub fn pop<'a>(
        &mut self,
        queue: &mut TaskQueue,
        partitions: NonZeroUsize,
        max: usize,
    ) -> Option<&'a mut Task> {
        if self.size == 0 {
            return None;
        }

        // Safe to "unwrap()" since size should be guaranteed by push() above
        let mut size = ((self.size / partitions.get()) + 1)
            .max(max)
            .max(TaskQueue::SIZE / 2);
        debug_assert!(size != 0);
        self.size -= size;
        let task = self.list.pop();

        // push all the other tasks without synchronization.
        size -= 1;
        if size != 0 {
            let mut tail = queue.unsync_load_tail();
            let mut head = queue.head().load(Ordering::Relaxed);
            debug_assert_eq!(
                tail.wrapping_sub(head),
                0,
                "Attempting to pop many from injector with a non-empty queue"
            );

            // move the batch of tasks into the provided queue
            for _ in 0..size {
                let task = match self.list.pop() {
                    Some(task) => task,
                    None => break,
                };
                match task.priority() {
                    TaskPriority::Low | TaskPriority::Normal => {
                        queue.tasks[(tail as usize) % TaskQueue::SIZE] = MaybeUninit::new(task);
                        tail = tail.wrapping_add(1);
                    }
                    TaskPriority::High => {
                        head = head.wrapping_sub(1);
                        queue.tasks[(head as usize) % TaskQueue::SIZE] = MaybeUninit::new(task);
                    }
                }
            }

            // Make the new tasks in the queue available to itself & consumers
            queue
                .pos
                .store(TaskQueue::to_pos(head, tail), Ordering::Release);
        }

        task
    }
}

/// A Bounded Multi-Consumer Single-Producer Queue
/// of tasks which supports batched stealing.
pub(super) struct TaskQueue {
    pos: AtomicUsize,
    tasks: [MaybeUninit<*mut Task>; Self::SIZE],
}

impl Default for TaskQueue {
    fn default() -> Self {
        Self {
            pos: AtomicUsize::new(0),
            tasks: [MaybeUninit::uninit(); Self::SIZE],
        }
    }
}

#[cfg(target_pointer_width = "64")]
type TaskQueueIndex = u32;
#[cfg(target_pointer_width = "64")]
type TaskQueueAtomicIndex = core::sync::atomic::AtomicU32;

#[cfg(target_pointer_width = "32")]
type TaskQueueIndex = u16;
#[cfg(target_pointer_width = "32")]
type TaskQueueAtomicIndex = core::sync::atomic::AtomicU16;

impl TaskQueue {
    /// The size of the TaskQueue ring buffer
    pub const SIZE: usize = 256;

    /// Get a reference to the atomic head index of the TaskQueue ring buffer
    #[inline(always)]
    pub fn head(&self) -> &TaskQueueAtomicIndex {
        &self.indices()[0]
    }

    /// Get a reference to the atomic tail index of the TaskQueue ring buffer
    #[inline(always)]
    pub fn tail(&self) -> &TaskQueueAtomicIndex {
        &self.indices()[1]
    }

    /// Split the atomic pos field into two for head & tail.
    /// Safe to do since `pos` acts as a C union.
    #[inline(always)]
    fn indices(&self) -> &[TaskQueueAtomicIndex; 2] {
        unsafe { &*(&self.pos as *const _ as *const _) }
    }

    /// Convert a head index and a tail index into a single value for `pos`.
    pub fn to_pos(head: TaskQueueIndex, tail: TaskQueueIndex) -> usize {
        unsafe { mem::transmute([head, tail]) }
    }

    /// Load the tail index without any synchronization.
    /// Safe to do so since it requires ownership and other threads cannot modify the tail.
    pub fn unsync_load_tail(&mut self) -> TaskQueueIndex {
        let tail_ref = self.tail() as *const _ as *mut TaskQueueAtomicIndex;
        unsafe { *(&mut *tail_ref).get_mut() }
    }

    /// Returns the number of tasks in the queue
    pub fn len(&self) -> usize {
        let tail = self.tail().load(Ordering::Acquire);
        let head = self.head().load(Ordering::Acquire);
        tail.wrapping_sub(head) as usize
    }

    /// Enqueue a task to the front or back of the ring buffer based on priority.
    /// Overflows half of the queue into injector when full.
    pub fn push<P: ThreadParker>(&mut self, task: &mut Task, injector: &RawMutex<TaskInjector, P>) {
        match task.priority() {
            TaskPriority::Low | TaskPriority::Normal => self.push_back(task, injector),
            TaskPriority::High => self.push_front(task, injector),
        }
    }

    /// Enqueue a task to the back of the ring buffer task queue
    fn push_back<P: ThreadParker>(
        &mut self,
        task: &mut Task,
        injector: &RawMutex<TaskInjector, P>,
    ) {
        loop {
            // unsychronized loads for both since:
            // - the tail is only modified by the queue owner
            // - needs no write visibility (Acquire) to self.tasks
            let tail = self.unsync_load_tail();
            let head = self.head().load(Ordering::Relaxed);

            // queue is not full, use Release to make self.tasks write available to stealers.
            if (tail.wrapping_sub(head) as usize) < Self::SIZE {
                self.tasks[(tail as usize) % Self::SIZE] = MaybeUninit::new(task);
                self.tail().store(tail.wrapping_add(1), Ordering::Release);
                return;
            }

            // queue is full, try to overflow into injector
            if self.push_overflow(task, head, injector) {
                return;
            } else {
                spin_loop_hint();
            }
        }
    }

    /// Enqueue a task to the front of the ring buffer task queue
    fn push_front<P: ThreadParker>(
        &mut self,
        task: &mut Task,
        injector: &RawMutex<TaskInjector, P>,
    ) {
        loop {
            // unsynchronized load on the tail since only we can modify it,
            // Relaxed load on the tail since not reading any self.tasks updates from other threads.
            let tail = self.unsync_load_tail();
            let head = self.head().load(Ordering::Relaxed);

            // queue is not full, use Release to make self.tasks write available to stealers.
            if (tail.wrapping_sub(head) as usize) < Self::SIZE {
                let new_head = head.wrapping_sub(1);
                self.tasks[(new_head as usize) % Self::SIZE] = MaybeUninit::new(task);
                match self.head().compare_exchange_weak(
                    head,
                    new_head,
                    Ordering::Release,
                    Ordering::Relaxed,
                ) {
                    Ok(_) => return,
                    Err(_) => {
                        spin_loop_hint();
                        continue;
                    }
                }
            }

            // queue is full, try to overflow into injector
            if self.push_overflow(task, head, injector) {
                return;
            } else {
                spin_loop_hint();
            }
        }
    }

    /// Migrate half of the queue's tasks into the injector to free up local lots.
    fn push_overflow<P: ThreadParker>(
        &mut self,
        task: &mut Task,
        head: TaskQueueIndex,
        injector: &RawMutex<TaskInjector, P>,
    ) -> bool {
        // the queue was full, try to move half of it to the injector
        // Relaxed orderings since no changes to self.tasks can be observed
        // (as we're the only producers).
        let batch = Self::SIZE / 2;
        if self
            .head()
            .compare_exchange_weak(
                head,
                head.wrapping_add(batch.try_into().unwrap()),
                Ordering::Relaxed,
                Ordering::Relaxed,
            )
            .is_err()
        {
            spin_loop_hint();
            return false;
        }

        // form a task list of the stolen tasks, prioritizing them internally.
        let mut batch_list = TaskList::default();
        batch_list.push(task);
        for i in 0..batch {
            let task = self.tasks[(head as usize).wrapping_add(i) % Self::SIZE];
            batch_list.push(unsafe { &mut *task.assume_init() });
        }

        // submit them all as a list to the injector
        injector.lock().push(batch_list);
        true
    }

    /// Dequeue a task from the front of the ring buffer
    pub fn pop<'a>(&mut self) -> Option<&'a mut Task> {
        loop {
            // unsynchronized tail load since only we can change it.
            // Relaxed head load since not observing self.tasks changes as no thread can push.
            let tail = self.unsync_load_tail();
            let head = self.head().load(Ordering::Relaxed);

            // the queue is empty
            if tail.wrapping_sub(head) == 0 {
                return None;
            }

            // try to consume & dequeue the task
            let task = self.tasks[(head as usize) % Self::SIZE];
            match self.head().compare_exchange_weak(
                head,
                head.wrapping_add(1),
                Ordering::Relaxed,
                Ordering::Relaxed,
            ) {
                Ok(_) => return Some(unsafe { &mut *task.assume_init() }),
                Err(_) => spin_loop_hint(),
            }
        }
    }

    /// Dequeue a task from the back of the ring buffer
    pub fn pop_back<'a>(&mut self) -> Option<&'a mut Task> {
        loop {
            // unsynchronized tail load since only we can change it.
            // Relaxed head load since no need to view self.tasks changes to head.
            let tail = self.unsync_load_tail();
            let head = self.head().load(Ordering::Relaxed);

            // the queue is empty
            if tail.wrapping_sub(head) == 0 {
                return None;
            }

            // try to consume & dequeue the task from the tail
            let new_tail = tail.wrapping_sub(1);
            let task = self.tasks[(new_tail as usize) % Self::SIZE];
            match self.pos.compare_exchange_weak(
                Self::to_pos(head, tail),
                Self::to_pos(head, new_tail),
                Ordering::Relaxed,
                Ordering::Relaxed,
            ) {
                Ok(_) => return Some(unsafe { &mut *task.assume_init() }),
                Err(_) => spin_loop_hint(),
            }
        }
    }

    /// Steal a batch of tasks from the `other` into our own queue,
    /// returning the first task stolen if any.
    pub fn steal<'a>(&mut self, other: &Self) -> Option<&'a mut Task> {
        let our_tail = self.unsync_load_tail();
        let our_head = self.head().load(Ordering::Relaxed);
        debug_assert_eq!(
            our_tail.wrapping_sub(our_head),
            0,
            "Should only steal if queue is empty"
        );

        // Acquire loads to other's head & tail to observe other.task's writes.
        loop {
            let tail = other.tail().load(Ordering::Acquire);
            let head = other.head().load(Ordering::Acquire);
            let size = tail.wrapping_sub(head);
            let mut batch = size - (size / 2);

            // other's queue is empty
            if batch == 0 {
                return None;
            }

            // read a batch (half) of other's tasks into ours
            for i in 0..batch {
                let task = other.tasks[(head.wrapping_add(i) as usize) % Self::SIZE];
                self.tasks[(our_tail.wrapping_add(i) as usize) % Self::SIZE] = task;
            }

            // try to commit the steal, use Relaxed since CAS acts as a release.
            if other
                .head()
                .compare_exchange_weak(
                    head,
                    head.wrapping_add(batch),
                    Ordering::Relaxed,
                    Ordering::Relaxed,
                )
                .is_err()
            {
                spin_loop_hint();
                continue;
            }

            // make our stolen tasks available & return the last one
            batch -= 1;
            if batch != 0 {
                self.tail()
                    .store(our_tail.wrapping_add(batch), Ordering::Release);
            }
            let task = self.tasks[(our_tail.wrapping_add(batch) as usize) % Self::SIZE];
            return Some(unsafe { &mut *task.assume_init() });
        }
    }
}
