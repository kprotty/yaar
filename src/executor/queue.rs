// Copyright 2019-2020 kprotty
//
// Licensed under the Apache License, Version 2.0, <LICENSE-APACHE or
// http://apache.org/licenses/LICENSE-2.0> or the MIT license <LICENSE-MIT or
// http://opensource.org/licenses/MIT>, at your option. This file may not be
// copied, modified, or distributed except according to those terms.

use super::{Task, TaskBatch};
use core::{
    fmt,
    cell::UnsafeCell,
    marker::PhantomPinned,
    pin::Pin,
    ptr::NonNull,
    sync::atomic::{AtomicPtr, AtomicUsize, Ordering},
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
        unimplemented!("TODO")
    }

    /// 
    #[inline]
    fn stub_ptr(&self) -> NonNull<Task> {
        NonNull::new(&self.stub as *const _ as *mut Task).unwrap()
    }

    pub fn is_empty(&self) -> bool {
        unimplemented!("TODO")
    }

    pub fn push(&self, batch: TaskBatch) {
        unimplemented!("TODO")
    }

    fn try_pop(&self) -> Option<GlobalQueueReceiver<'_>> {
        unimplemented!("TODO")
    }
}

/// A GlobalQueueReceiver represents ownership of the dequeue end of a GlobalQueue.
/// As long as this object is alive, no other task can dequeue from the GlobalQueue.
/// This allows the global queue to be implemented with a Single-Consumer for performance.
///
/// Dequeueing from a GlobalQueue is modeled as an iterator of tasks that it sees available.
/// Due to the nature of the underlying algorithm, it may lose and regain visibility of tasks at any time.
/// This means that polling for new tasks after [`GlobalQueueReceiver::next`] returns `None` is still a valid strategy.
struct GlobalQueueReceiver<'a> {
    queue: &'a GlobalQueue,
    tail: NonNull<Task>,
}

impl<'a> Iterator for GlobalQueueReceiver<'a> {
    type Item = NonNull<Task>;

    fn next(&mut self) -> Option<Self::Item> {
        unimplemented!("TODO")
    }
}

/// A LocalQueue is a bounded Single-Producer-Multi-Consumer queue of tasks.
/// It is backed by a FIFO ring buffer + a LIFO task slot.
pub struct LocalQueue {
    next: AtomicPtr<Task>,
    head: AtomicUsize,
    tail: AtomicUsize,
    buffer: [MaybeUninit<AtomicPtr<Task>>; Self::BUFFER_SIZE],
}

impl fmt::Debug for LocalQueue {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("LocalQueue")
            .field("is_empty", &self.is_empty())
            .finish()
    }
}

impl Default for LocalQueue {
    fn default() -> Self {
        Self::new()
    }
}

impl LocalQueue {
    const BUFFER_SIZE: usize = 256;

    /// Creates a new, empty, local queue/
    pub fn new() -> Self {
        Self {
            next: AtomicPtr::default(),
            head: AtomicUsize::default(),
            tail: AtomicUsize::default(),
            buffer: unsafe { MaybeUninit::uninit().assume_init() },
        }
    }
    
    /// Returns true if this local queue is observed to be empty
    pub fn is_empty(&self) -> bool {
        loop {
            // load the tail, head, and next in order.
            // Acquire barrier to make sure any loads here dont get re-ordered.
            let tail = self.tail.load(Ordering::Acquire);
            let head = self.head.load(Ordering::Acquire);
            let next = self.next.load(Ordering::Acquire);

            // need to recheck the tail to make sure that the buffer didn't change in the mean time.
            if tail == self.tail.load(Ordering::Relaxed) {
                return head == tail && next.is_null();
            }
        }
    }

    #[inline]
    unsafe fn read(&self, index: usize) -> NonNull<Task> {

    }

    #[inline]
    unsafe fn write(&self, index: usize, task: NonNull<Task>) {
        
    }

    /// Enqueue a batch of tasks to this local buffer.
    /// If `push_next` then tries to push one of the batch tasks to the LIFO slot.
    /// If there's more tasks in the batch than there are in the buffer,
    ///     if overflows half of the buffer's tasks into a batch as an `Err`.
    ///
    /// # Safety
    ///
    /// This method must only be called by the producer thread of this LocalQueue
    pub unsafe fn try_push(
        &self,
        push_next: bool,
        mut batch: TaskBatch,
    ) -> Result<(), TaskBatch> {
        // Try to push the first task of the batch to the self.next LIFO slot
        let mut has_next_task = false;
        if push_next {
            if let Some(task) = batch.pop() {
                let mut next = self.next.load(Ordering::Relaxed);
                loop {
                    // If theres already a task in the next slot, then replace it with the new task.
                    // Release ordering on success to ensure stealer threads see valid task field writes.
                    if let Some(mut old_next) = NonNull::new(next) {
                        match self.next.compare_exchange_weak(
                            old_next.as_ptr(),
                            task.as_ptr(),
                            Ordering::Release,
                            Ordering::Relaxed,
                        ) {
                            Err(e) => next = e,
                            Ok(_) => {
                                batch.push_front(Pin::new_unchecked(old_next.as_mut()));
                                has_next_task = true;
                                break;
                            }
                        }

                    // The next slot is empty, move the task into it without a bus lock instruction.
                    // Release ordering on success to ensure stealer threads see valid task field writes.
                    } else {
                        self.next.store(task.as_ptr(), Ordering::Release);
                        break;
                    }
                }
            }
        }

        // Prepare to update our local buffer with tasks from the task batch.
        // The tail load here could be unsynchronized since we're the only thread that updates it.
        let mut tail = self.tail.load(Ordering::Relaxed);
        let mut head = self.head.load(Ordering::Relaxed);
        loop {
            let size = tail.wrapping_sub(head);
            assert!(
                size <= Self::BUFFER_SIZE,
                "invalid local queue size of {}",
                size,
            );
            
            // If theres space in our local buffer, try to push tasks from the batch there.
            // Store tasks to the buffer using atomics as a stealer thread could be simulaniously reading.
            let remaining = Self::BUFFER_SIZE - size;
            if remaining != 0 {
                let new_tail = core::iter::repeat_with(|| batch.pop())
                    .filter_map(|task| task)
                    .take(remaining)
                    .fold(tail, |tail, task| {
                        let slot = self.buffer.get_unchecked(tail % Self::BUFFER_SIZE);
                        (*slot.as_ptr()).store(task.as_ptr(), Ordering::Relaxed);
                        tail.wrapping_add(1)
                    });
                
                // Update the tail of the run queue after writing tasks to it.
                // After the update, re-check the head to see if any stealers made more space.
                // Note that it is O.K. if the head load is re-ordered above the tail store here.
                // Release barrier to ensure that stealer threads see the writes to the buffer above.
                if tail != new_tail {
                    self.tail.store(new_tail, Ordering::Release);
                    head = self.head.load(Ordering::Relaxed);
                    tail = new_tail;
                    continue;
                
                // Nothing left to push so it was successfull
                } else if batch.len() == 0 {
                    return Ok(());
                }
            }
            
            // Try to steal half of the tasks out of the local queue to make future pushes succeed.
            // Acquire barrier so that the buffer reads below aren't re-ordered before the steal succeeds.
            let steal = Self::BUFFER_SIZE / 2;
            if let Err(e) = self.head.compare_exchange_weak(
                head,
                head.wrapping_add(steal),
                Ordering::Acquire,
                Ordering::Relaxed,
            ) {
                head = e;
                continue;
            }

            // Managed to mark half the tasks in the buffer as stolen, turn them into a batch.
            // No need to do a racy read as seen in [`try_steal_from_local`] since we're the only producers.
            let mut overflow_batch = TaskBatch::new();
            for offset in 0..steal {
                let index = head.wrapping_add(offset);
                let slot = self.buffer.get_unchecked(index % Self::BUFFER_SIZE);
                let task = (*slot.as_ptr()).load(Ordering::Relaxed);
                overflow_batch.push_back(Pin::new_unchecked(&mut *task));
            }
            
            // Append the pushed batch after the stolen tasks since they were in the buffer already.
            overflow_batch.push_back(batch);
            return Err(overflow_batch);
        }
    }

    /// Try to dequeue a task from this local buffer.
    /// If `pop_next` then the LIFO slot will be checked & dequeued for a task.
    ///
    /// # Safety
    ///
    /// This method must only be called by the producer thread of this LocalQueue
    pub unsafe fn try_pop(&self, pop_next: bool) -> Option<NonNull<Task>> {
        // If pop_next, try to consume the LIFO `next` slot if it has a task.
        // No memory barriers are necessarily needed here since all task writes originated on this thread.
        if pop_next {
            let mut next = self.next.load(Ordering::Relaxed);
            while let Some(next_task) = NonNull::new(next) {
                match self.next.compare_exchange_weak(
                    next,
                    ptr::null_mut(),
                    Ordering::Relaxed,
                    Ordering::Relaxed,
                ) {
                    Ok(_) => return Some(next_task),
                    Err(e) => next = e,
                }
            }
        }

        // Try to dequeue a task from the local buffer.
        // Note that the tail load here could be unsynchronized.
        let tail = self.tail.load(Ordering::Relaxed);
        let mut head = self.head.load(Ordering::Relaxed);
        loop {
            let size = tail.wrapping_sub(head);
            assert!(
                size <= Self::BUFFER_SIZE,
                "invalid local queue size of {}",
                size,
            );
            
            // If the local queue is empty, nothing left to poll locally.
            if size == 0 {
                return None;
            }

            // Try to steal one task off from the front of the local queue.
            // No memory barriers as its OK if the task read below gets re-orderd above this CAS.
            // It is ok because we are the only thread which is able to write to the local buffer.
            if let Err(e) = self.head.compare_exchange_weak(
                head,
                head.wrapping_add(1),
                Ordering::Relaxed,
                Ordering::Relaxed,
            ) {
                head = e;
                continue;
            }

            let slot = 
        }
    }

    /// Try to dequeue a batch of tasks from the target LocalQueue buffer.
    ///
    /// # Safety
    ///
    /// This method must only be called by the producer thread of this LocalQueue.
    /// The `target` LocalQueue that is being stolen from must not be/alias-to `self`.
    pub unsafe fn try_steal_from_local(&self, target: &Self) -> Option<NonNull<Task>> {
        unimplemented!("TODO")
    }

    ///
    /// # Safety
    ///
    /// This method must only be called by the producer thread of this LocalQueue
    pub unsafe fn try_steal_from_global(&self, target: &GlobalQueue) -> Option<NonNull<Task>> {
        unimplemented!("TODO")
    }
}
