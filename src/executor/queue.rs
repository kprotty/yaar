use super::{super::sync::CachePadded, Batch, Task};
use core::{
    hint::unreachable_unchecked,
    mem::MaybeUninit,
    pin::Pin,
    ptr::{self, NonNull},
    sync::atomic::{spin_loop_hint, AtomicUsize, Ordering},
};

pub struct GlobalQueue {
    head: CachePadded<AtomicUsize>,
}

impl Default for GlobalQueue {
    fn default() -> Self {
        Self::new()
    }
}

impl Drop for GlobalQueue {
    fn drop(&mut self) {
        debug_assert_eq!(*self.head.get_mut(), 0);
    }
}

impl GlobalQueue {
    pub const fn new() -> Self {
        GlobalQueue {
            head: CachePadded::new(AtomicUsize::new(0)),
        }
    }

    pub fn push(&self, mut batch: Batch) {
        if let Some(batch_head) = batch.head {
            let mut head = self.head.load(Ordering::Relaxed);
            loop {
                unsafe { *batch.tail.as_mut().next.get_mut() = head };
                match self.head.compare_exchange_weak(
                    head,
                    batch_head.as_ptr() as usize,
                    Ordering::Release,
                    Ordering::Relaxed,
                ) {
                    Ok(_) => return,
                    Err(e) => head = e,
                }
            }
        }
    }
}

pub struct LocalQueue {
    head: AtomicUsize,
    tail: AtomicUsize,
    overflow: AtomicUsize,
    buffer: CachePadded<[AtomicUsize; Self::BUFFER_SIZE]>,
}

impl Default for LocalQueue {
    fn default() -> Self {
        Self::new()
    }
}

impl Drop for LocalQueue {
    fn drop(&mut self) {
        debug_assert_eq!(*self.tail.get_mut(), *self.head.get_mut());
        debug_assert_eq!(*self.overflow.get_mut(), 0);
    }
}

impl LocalQueue {
    const BUFFER_SIZE: usize = 256;

    pub fn new() -> Self {
        Self {
            head: AtomicUsize::new(0),
            tail: AtomicUsize::new(0),
            overflow: AtomicUsize::new(0),
            buffer: CachePadded::new(unsafe {
                let mut buffer: MaybeUninit<[_; Self::BUFFER_SIZE]> =
                    MaybeUninit::uninit();
                let buffer_ptr = buffer.as_mut_ptr() as *mut AtomicUsize;
                for i in 0..Self::BUFFER_SIZE {
                    ptr::write(buffer_ptr.add(i), AtomicUsize::new(0));
                }
                buffer.assume_init()
            }),
        }
    }

    unsafe fn read(&self, index: usize) -> NonNull<Task> {
        let slot = self.buffer.get_unchecked(index % Self::BUFFER_SIZE);
        let ptr = slot.load(Ordering::Relaxed);
        NonNull::new_unchecked(ptr as *mut Task)
    }

    unsafe fn write(&self, index: usize, value: NonNull<Task>) {
        let slot = self.buffer.get_unchecked(index % Self::BUFFER_SIZE);
        let ptr = value.as_ptr() as usize;
        slot.store(ptr, Ordering::Relaxed);
    }

    pub unsafe fn push(&self, mut batch: Batch) {
        let mut tail = self.tail.load(Ordering::Relaxed);
        let mut head = self.head.load(Ordering::Relaxed);

        while !batch.is_empty() {
            let size = tail.wrapping_sub(head);
            assert!(size <= Self::BUFFER_SIZE, "invalid queue size");

            let remaining = Self::BUFFER_SIZE - size;
            if remaining > 0 {
                let new_tail =
                    batch.drain().take(remaining).fold(tail, |new_tail, task| {
                        self.write(new_tail, task);
                        new_tail.wrapping_add(1)
                    });

                if new_tail != tail {
                    tail = new_tail;
                    self.tail.store(new_tail, Ordering::Release);
                }

                head = self.head.load(Ordering::Relaxed);
                continue;
            }

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

            let mut overflowed_batch = (0..steal)
                .map(|i| self.read(head.wrapping_add(i)))
                .chain(batch.into_iter())
                .map(|task| Pin::new_unchecked(&mut *task.as_ptr()))
                .collect::<Batch>();

            let mut overflow = self.overflow.load(Ordering::Relaxed);
            loop {
                *overflowed_batch.tail.as_mut().next.get_mut() = overflow;
                let new_overflow = overflowed_batch
                    .head
                    .unwrap_or_else(|| unreachable_unchecked())
                    .as_ptr() as usize;

                if overflow == 0 {
                    self.overflow.store(new_overflow, Ordering::Release);
                    return;
                }

                match self.overflow.compare_exchange_weak(
                    overflow,
                    new_overflow,
                    Ordering::Release,
                    Ordering::Relaxed,
                ) {
                    Ok(_) => return,
                    Err(e) => overflow = e,
                }
            }
        }
    }

    pub unsafe fn try_pop(&self) -> (Option<NonNull<Task>>, bool) {
        let tail = self.tail.load(Ordering::Relaxed);
        let mut head = self.head.load(Ordering::Relaxed);

        loop {
            let size = tail.wrapping_sub(head);
            assert!(size <= Self::BUFFER_SIZE, "invalid run queue size");

            if size == 0 {
                break;
            }

            match self.head.compare_exchange_weak(
                head,
                head.wrapping_add(1),
                Ordering::Relaxed,
                Ordering::Relaxed,
            ) {
                Ok(_) => return (Some(self.read(head)), false),
                Err(e) => head = e,
            }
        }

        let mut overflow = self.overflow.load(Ordering::Relaxed);
        while overflow != 0 {
            match self.overflow.compare_exchange_weak(
                overflow,
                0,
                Ordering::Relaxed,
                Ordering::Relaxed,
            ) {
                Ok(_) => return self.inject(tail, overflow),
                Err(e) => overflow = e,
            }
        }

        (None, false)
    }

    pub unsafe fn try_steal_from_local(
        &self,
        target: &Self,
    ) -> (Option<NonNull<Task>>, bool) {
        let tail = self.tail.load(Ordering::Relaxed);
        let head = self.head.load(Ordering::Relaxed);
        assert_eq!(tail, head, "non empty runq when stealing from local");

        let mut target_head = target.head.load(Ordering::Relaxed);
        loop {
            let target_tail = target.tail.load(Ordering::Acquire);
            let target_size = target_tail.wrapping_sub(target_head);
            let steal = target_size - (target_size / 2);

            if steal > Self::BUFFER_SIZE {
                spin_loop_hint();
                target_head = target.head.load(Ordering::Relaxed);
                continue;
            }

            if steal == 0 {
                let target_overflow = target.overflow.load(Ordering::Relaxed);
                if target_overflow == 0 {
                    break;
                }

                match target.overflow.compare_exchange_weak(
                    target_overflow,
                    0,
                    Ordering::Acquire,
                    Ordering::Relaxed,
                ) {
                    Ok(_) => return self.inject(tail, target_overflow),
                    Err(_) => {
                        spin_loop_hint();
                        target_head = target.head.load(Ordering::Relaxed);
                        continue;
                    }
                }
            }

            let mut new_target_head = target_head;
            let mut target_tasks = (0..steal).map(|_| {
                let task = target.read(new_target_head);
                new_target_head = new_target_head.wrapping_add(1);
                task
            });

            let first_task = target_tasks.next();
            let new_tail = target_tasks.fold(tail, |new_tail, task| {
                self.write(new_tail, task);
                new_tail.wrapping_add(1)
            });

            if let Err(e) = target.head.compare_exchange_weak(
                target_head,
                new_target_head,
                Ordering::Release,
                Ordering::Relaxed,
            ) {
                target_head = e;
                continue;
            }

            let has_more = new_tail != tail;
            if has_more {
                self.tail.store(new_tail, Ordering::Release);
            }

            return (first_task, has_more);
        }

        (None, false)
    }

    pub unsafe fn try_steal_from_global(&self, target: &GlobalQueue) -> (Option<NonNull<Task>>, bool) {
        let mut task_ptr = target.load(Ordering::)
    }

    unsafe fn inject(
        &self,
        tail: usize,
        mut task_ptr: usize,
    ) -> (Option<NonNull<Task>>, bool) {
        let mut task_iter = core::iter::from_fn(|| {
            NonNull::new(task_ptr as *mut Task).map(|mut task| {
                task_ptr = *task.as_mut().next.get_mut();
                task
            })
        });

        let first_task = task_iter.next();

        let new_tail = task_iter
            .take(Self::BUFFER_SIZE)
            .fold(tail, |new_tail, task| {
                self.write(new_tail, task);
                new_tail.wrapping_sub(1)
            });

        if new_tail != tail {
            self.tail.store(new_tail, Ordering::Release);
        }

        let has_more = task_ptr != 0;
        if has_more {
            let overflow = self.overflow.load(Ordering::Relaxed);
            assert_eq!(overflow, 0, "non empty overflow when injecting");
            self.overflow.store(task_ptr, Ordering::Release);
        }

        (first_task, has_more)
    }
}
