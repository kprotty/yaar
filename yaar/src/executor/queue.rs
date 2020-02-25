use super::{TaskPriority, TaskQueue, Task, TaskList};
use core::{
    cell::{Cell, UnsafeCell},
    convert::TryInto,
    hint::unreachable_unchecked,
    mem::{size_of, transmute, MaybeUninit},
    num::NonZeroUsize,
    ptr::NonNull,
    sync::atomic::{spin_loop_hint, AtomicUsize, Ordering},
};
use crossbeam_utils::{atomic::AtomicConsume, CachePadded};
use lock_api::RawMutex;

pub struct GlobalQueue<Mutex: RawMutex> {
    mutex: CachePadded<Mutex>,
    pub size: AtomicUsize,
    list: UnsafeCell<TaskQueue>,
}

impl<Mutex: RawMutex> Default for GlobalQueue<Mutex> {
    fn default() -> Self {
        Self {
            mutex: CachePadded::new(Mutex::INIT),
            size: AtomicUsize::new(0),
            list: UnsafeCell::new(TaskQueue::default()),
        }
    }
}

impl<Mutex: RawMutex> GlobalQueue<Mutex> {
    fn unsync_load_size(&self) -> usize {
        unsafe { *(&self.size as *const _ as *const _) }
    }

    pub fn len(&self) -> usize {
        self.size.load_consume()
    }

    pub fn push(&self, batch: TaskList) {
        let TaskList { front, back, size } = batch;
        if size == 0 {
            return;
        }

        unsafe {
            self.mutex.lock();

            let list = &mut *self.list.get();
            list.push_front(front);
            list.push_back(back);
            self.size
                .store(self.unsync_load_size() + size, Ordering::Release);

            self.mutex.unlock();
        }
    }

    pub unsafe fn pop(
        &self,
        local: &LocalQueue,
        distribute: NonZeroUsize,
        max_batch: NonZeroUsize,
    ) -> Option<NonNull<Task>> {
        let pos = local.pos.load(Ordering::Relaxed);
        let [head, mut tail] = LocalQueue::from_pos(pos);
        if max_batch.get() != 1 {
            debug_assert_eq!(
                tail.wrapping_sub(head),
                0,
                "Should only pop many if local queue is empty",
            );
        }

        self.mutex.lock();

        let size = self.unsync_load_size();
        if size == 0 {
            self.mutex.unlock();
            return None;
        }

        let mut batch = ((size / distribute.get()) + 1)
            .max(max_batch.get())
            .min(LocalQueue::SIZE);
        self.size.store(size + batch, Ordering::Release);

        let list = &mut *self.list.get();
        let task = list.pop();
        batch -= 1;

        if batch != 0 {
            for i in 0..batch {
                let task = list.pop().unwrap_or_else(|| unreachable_unchecked());
                let index = (tail as usize).wrapping_add(i) % LocalQueue::SIZE;
                local.tasks[index].set(MaybeUninit::new(task));
                tail = tail.wrapping_add(1);
            }
            let [_, tail_pos] = local.positions();
            tail_pos.store(tail, Ordering::Release);
        }

        self.mutex.unlock();
        task
    }
}

use self::atomic_index::*;

#[cfg(target_pointer_width = "64")]
mod atomic_index {
    pub type AtomicIndex = core::sync::atomic::AtomicU32;
    pub type Index = u32;
}

#[cfg(target_pointer_width = "32")]
mod atomic_index {
    pub type AtomicIndex = core::sync::atomic::AtomicU16;
    pub type Index = u16;
}

pub struct LocalQueue {
    pos: AtomicUsize,
    tasks: CachePadded<[Cell<MaybeUninit<NonNull<Task>>>; Self::SIZE]>,
}

impl Default for LocalQueue {
    fn default() -> Self {
        Self {
            pos: AtomicUsize::new(0),
            tasks: CachePadded::new(unsafe {
                let mut tasks = MaybeUninit::uninit();
                let ptr = tasks.as_mut_ptr() as *mut Cell<MaybeUninit<NonNull<Task>>>;
                for i in 0..Self::SIZE {
                    *ptr.add(i) = Cell::new(MaybeUninit::uninit());
                }
                tasks.assume_init()
            }),
        }
    }
}

impl LocalQueue {
    const SIZE: usize = size_of::<usize>() * 32;

    fn positions(&self) -> &[AtomicIndex; 2] {
        unsafe { transmute(&self.pos) }
    }

    fn from_pos(pos: usize) -> [Index; 2] {
        unsafe { transmute(pos) }
    }

    fn to_pos(head: Index, tail: Index) -> usize {
        unsafe { transmute([head, tail]) }
    }

    pub fn len(&self) -> usize {
        let pos = self.pos.load_consume();
        let [head, tail] = Self::from_pos(pos);
        tail.wrapping_sub(head) as usize
    }

    pub unsafe fn push<M: RawMutex>(&self, task: NonNull<Task>, global: &GlobalQueue<M>) {
        match task.as_ref().priority() {
            TaskPriority::Low | TaskPriority::Normal => self.push_back(task, global),
            TaskPriority::High | TaskPriority::Critical => self.push_front(task, global),
        }
    }

    fn push_back<M: RawMutex>(&self, task: NonNull<Task>, global: &GlobalQueue<M>) {
        let pos = self.pos.load(Ordering::Relaxed);
        let [mut head, tail] = Self::from_pos(pos);
        let [_, tail_pos] = self.positions();

        loop {
            if (tail.wrapping_sub(head) as usize) < Self::SIZE {
                self.tasks[(tail as usize) % Self::SIZE].set(MaybeUninit::new(task));
                tail_pos.store(tail.wrapping_add(1), Ordering::Release);
                return;
            }

            match self.push_overflow(head, task, global) {
                Ok(_) => return,
                Err(new_head) => {
                    head = new_head;
                    spin_loop_hint();
                }
            }
        }
    }

    fn push_front<M: RawMutex>(&self, task: NonNull<Task>, global: &GlobalQueue<M>) {
        let pos = self.pos.load(Ordering::Relaxed);
        let [mut head, tail] = Self::from_pos(pos);
        let [head_pos, _] = self.positions();

        loop {
            if (tail.wrapping_sub(head) as usize) < Self::SIZE {
                let new_head = head.wrapping_sub(1);
                self.tasks[(new_head as usize) % Self::SIZE].set(MaybeUninit::new(task));
                match head_pos.compare_exchange_weak(
                    head,
                    new_head,
                    Ordering::Release,
                    Ordering::Relaxed,
                ) {
                    Ok(_) => return,
                    Err(new_head) => {
                        head = new_head;
                        spin_loop_hint();
                        continue;
                    }
                }
            }

            match self.push_overflow(head, task, global) {
                Ok(_) => return,
                Err(new_head) => {
                    head = new_head;
                    spin_loop_hint();
                }
            }
        }
    }

    fn push_overflow<M: RawMutex>(
        &self,
        head: Index,
        task: NonNull<Task>,
        global: &GlobalQueue<M>,
    ) -> Result<(), Index> {
        let batch = Self::SIZE / 2;
        let [head_pos, _] = self.positions();
        head_pos.compare_exchange_weak(
            head,
            head.wrapping_add(batch.try_into().unwrap()),
            Ordering::Relaxed,
            Ordering::Relaxed,
        )?;

        let mut list = TaskList::default();
        unsafe {
            list.push(task);
            for i in 0..batch {
                let index = (head as usize).wrapping_add(i);
                let task = self.tasks[index % Self::SIZE].get();
                list.push(task.assume_init());
            }
        }

        global.push(list);
        Ok(())
    }

    pub unsafe fn pop(&self) -> Option<NonNull<Task>> {
        let pos = self.pos.load(Ordering::Relaxed);
        let [mut head, tail] = Self::from_pos(pos);
        let [head_pos, _] = self.positions();

        loop {
            if tail.wrapping_sub(head) == 0 {
                return None;
            }

            let task = self.tasks[(head as usize) % Self::SIZE].get();
            match head_pos.compare_exchange_weak(
                head,
                head.wrapping_add(1),
                Ordering::Relaxed,
                Ordering::Relaxed,
            ) {
                Ok(_) => return Some(task.assume_init()),
                Err(new_head) => {
                    head = new_head;
                    spin_loop_hint();
                    continue;
                }
            }
        }
    }

    pub unsafe fn pop_back(&self) -> Option<NonNull<Task>> {
        let mut pos = self.pos.load(Ordering::Relaxed);

        loop {
            let [head, tail] = Self::from_pos(pos);
            if tail.wrapping_sub(head) == 0 {
                return None;
            }

            let new_tail = tail.wrapping_sub(1);
            let task = self.tasks[(new_tail as usize) % Self::SIZE].get();
            match self.pos.compare_exchange_weak(
                pos,
                Self::to_pos(head, tail),
                Ordering::Relaxed,
                Ordering::Relaxed,
            ) {
                Ok(_) => return Some(task.assume_init()),
                Err(new_pos) => {
                    pos = new_pos;
                    spin_loop_hint();
                    continue;
                }
            }
        }
    }

    pub fn steal(&self, victim: &Self) -> Option<NonNull<Task>> {
        let pos = self.pos.load(Ordering::Relaxed);
        let [head, tail] = Self::from_pos(pos);
        debug_assert_eq!(tail.wrapping_sub(head), 0, "Should only steal if empty");

        loop {
            let victim_pos = victim.pos.load_consume();
            let [victim_head, victim_tail] = Self::from_pos(victim_pos);

            let mut batch = victim_tail.wrapping_sub(victim_head);
            batch = batch - (batch / 2);
            if batch == 0 {
                return None;
            }

            for i in 0..batch {
                let index = victim_head.wrapping_add(i) as usize;
                let task = victim.tasks[index % Self::SIZE].get();
                self.tasks[(tail.wrapping_add(i) as usize) % Self::SIZE].set(task);
            }

            let [victim_head_pos, _] = victim.positions();
            if victim_head_pos
                .compare_exchange_weak(
                    victim_head,
                    victim_head.wrapping_add(batch),
                    Ordering::Relaxed,
                    Ordering::Relaxed,
                )
                .is_err()
            {
                spin_loop_hint();
                continue;
            }

            batch -= 1;
            let new_tail = tail.wrapping_add(batch);
            if batch != 0 {
                let [_, tail_pos] = self.positions();
                tail_pos.store(new_tail, Ordering::Release);
            }

            let task = self.tasks[(new_tail as usize) % Self::SIZE].get();
            return Some(unsafe { task.assume_init() });
        }
    }
}
