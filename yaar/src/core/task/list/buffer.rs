use core::{
    num::NonZeroUsize,
    mem::MaybeUninit,
    ptr::{self, NonNull},
    sync::atomic::{Ordering, AtomicUsize},
};
use super::super::{Task, TaskPriority};

#[cfg(any(
    target_pointer_width = "64",
    target_pointer_width = "32",
))]
use atomics::{Pos, AtomicPos};

#[cfg(target_pointer_width = "64")]
mod atomics {
    pub type Pos = u32;
    pub type AtomicPos = core::sync::atomic::AtomicU32;
}

#[cfg(target_pointer_width = "32")]
mod atomics {
    pub type Pos = u16;
    pub type AtomicPos = core::sync::atomic::AtomicU16;
}

pub struct TaskBuffer {
    array: MaybeUninit<[NonNull<Task>; Self::SIZE]>,
}

impl Default for TaskBuffer {
    fn default() -> Self {
        Self::new()
    }
}

impl TaskBuffer {
    const SIZE: usize = 128;

    #[inline]
    pub fn new() -> Self {
        Self {
            array: unsafe { MaybeUninit::uninit().assume_init() },
        }
    }

    #[inline]
    pub fn len(&self) -> usize {
        self.array.len()
    }

    #[inline]
    pub unsafe fn wrapping_read(&self, index) -> NonNull<Task> {
        let ptr = self.array.as_ptr() as *const NonNull<Task>;
        ptr::read(ptr.add(index % Self::SIZE))
    }

    #[inline]
    pub unsafe fn wrapping_write(&self, index: usize, task: NonNull<Task>) {
        let ptr = self.array.as_ptr() as *mut NonNull<Task>;
        ptr::write(ptr.add(index % Self::SIZE), task)
    }
}

pub struct TaskListBuffer {
    pos: CachePadded<AtomicUsize>,
    buffer: TaskBuffer,
}

impl Default for TaskListBuffer {
    fn default() -> Self {
        Self::new()
    }
}

impl TaskListBuffer {
    pub fn new() -> Self {
        Self {
            pos: CachePadded::new(AtomicUsize::new(0)),
            buffer: TaskBuffer::default(),
        }
    }

    #[inline]
    fn decode(pos: usize) -> [Pos; 2] {
        unsafe { mem::transmute(pos) }
    }

    #[inline]
    fn encode(head: Pos, tail: Pos) -> usize {
        unsafe { mem::transmute([head, tail]) }
    }

    #[inline]
    fn head(&self) -> &AtomicPos {
        unsafe { &*(&self.pos as *const _ as *const _).add(0) }
    }

    #[inline]
    fn tail(&self) -> &AtomicPos {
        unsafe { &*(&self.pos as *const _ as *const _).add(1) }
    }

    pub fn size(&self) -> usize {
        let pos = self.pos.load(Ordering::Relaxed);
        let [head, tail] = Self::decode(pos);
        tail.wrapping_sub(head) as usize
    } 

    pub unsafe fn push(&self, task: NonNull<Task>) -> Result<(), NonNull<Task>> {
        let (_, priority) = Task::decode(task);
        let push_back = match priority {
            TaskPriority::Low | TaskPriority::Normal => true,
            _ => false,
        };

        let mut pos = self.pos.load(Ordering::Relaxed);
        loop {
            let [head, tail] = Self::decode(pos);
            if tail.wrapping_sub(head) >= self.buffer.len() {
                return Err(task);
            }
                
            if push_back {
                let new_tail = tail.wrapping_add(1);
                self.buffer.wrapping_write(tail as usize, task);
                self.tail().store(new_tail, Ordering::Release);
                return Ok(());
            } else {
                let new_head = head.wrapping_sub(1);
                self.buffer.wrapping_write(new_head as usize, task);
                match self.pos.compare_exchange_weak(
                    pos,
                    Self::encode([new_head, tail]),
                    Ordering::Release,
                    Ordering::Relaxed,
                ) {
                    Ok(_) => return Ok(()),
                    Err(e) => pos = e,
                }
            }
        }
    }

    #[inline]
    pub unsafe fn pop_front(&self) -> Option<NonNull<Task>> {
        self.pop(false)
    }

    #[inline]
    pub unsafe fn pop_back(&self) -> Option<NonNull<Task>> {
        self.try_pop(true)
    }

    unsafe fn pop(&self, pop_back: bool) -> Option<NonNull<Task>> {
        let mut pos = self.pos.load(Ordering::Relaxed);
        loop {
            let [mut head, mut tail] = Self::decode(pos);
            if tail.wrapping_sub(head) == 0 {
                return None;
            }

            let task = if pop_back {
                tail = tail.wrapping_sub(1);
                self.buffer.wrapping_read(tail as usize)
            } else {
                let task = self.buffer.wrapping_read(head as usize);
                head = head.wrapping_add(1);
                task
            };

            match self.pos.compare_exchange_weak(
                pos,
                Self::encode(head, tail),
                Ordering::Relaxed,
                Ordering::Relaxed,
            ) {
                Ok(_) => return Some(task),
                Err(e) => pos = e,
            }
        }
    }

    pub unsafe fn steal_from(&self, target: &Self) -> Option<NonNull<Task>> {
        let pos = self.pos.load(Ordering::Relaxed);
        let [head, tail] = Self::decode(pos);

        let empty_slots = self.buffer.len() - (tail.wrapping_sub(head) as usize);
        let buffer_size = match NonZeroUsize::new(empty_slots as usize) {
            Some(buffer_size) => buffer_size,
            None => return None,
        };

        target.steal_into(
            tail as usize,
            buffer_size,
            &self.buffer,
            |size| Some(size.get() - (size.get() >> 1))
        ).map(|stole| {
            let mut stole = stole.get();
            let task = self.buffer.wrapping_read(tail.wrapping_add(stole) as usize);
            if stole > 1 {
                self.tail().store(tail.wrapping_add(stole - 1), Ordering::Release);
            }
        })
    }

    pub unsafe fn steal_into(
        &self,
        buffer_start: usize,
        buffer_size: NonZeroUsize,
        buffer: &TaskBuffer,
        steal_size: impl Fn(NonZeroUsize) -> Option<NonZeroUsize>,
    ) -> Option<NonZeroUsize> {
        let mut pos = self.pos.load(Ordering::Acquire);

        loop {
            let [head, tail] = Self::decode(pos);
            let size = tail.wrapping_sub(head) as usize;
            let size = match NonZeroUsize::new(size).and_then(steal_size) {
                Some(size) => size,
                None => return None,
            };

            let size = NonZeroUsize::new_unchecked(
                size
                .get()
                .min(buffer_size.get())
                .min(self.buffer.len()),
            );
            
            for i in 0..size.get() {
                let task = self.buffer.wrapping_read((head as usize).wrapping_add(i));
                buffer.wrapping_write(buffer_start.wrapping_add(i), task);
            }

            let new_head = head.wrapping_add(size
                .get()
                .try_into::<Pos>()
                .unwrap_unchecked()
            );

            match self.pos.compare_exchange_weak(
                pos,
                Self::encode([new_head, tail]),
                Ordering::Acquire, // could be Relaxed
                Ordering::Acquire,
            ) {
                Ok(_) => return Some(size),
                Err(e) => pos = e,
            }
        }
    }
}