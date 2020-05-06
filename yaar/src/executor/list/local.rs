use core::{
    num::NonZeroUsize,
    mem::MaybeUninit,
    ptr::{self, NonNull},
    sync::atomic::{Ordering, AtomicUsize},
};
use crate::executor::{
    Platform,
    task::{Task, TaskPriority},
};
use yaar_lock::utils::{
    Unsync,
    CachePadded,
};

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

pub struct LocalListBuffer<P: Platform> {
    array: MaybeUninit<[NonNull<Task<P>>; Self::SIZE]>,
}

impl<P: Platform> LocalListBuffer<P> {
    pub const SIZE: usize = 128;

    pub fn new() -> Self {
        Self {
            array: unsafe { MaybeUninit::uninit().assume_init() }
        }
    }

    #[inline]
    pub unsafe fn wrapping_read(&self, index: usize) -> NonNull<Task<P>> {

    }

    #[inline]
    pub unsafe fn wrapping_write(&self, index: usize, value: NonNull<Task<P>>) {
        
    }
}

pub struct LocalList<P: Platform> {
    pos: CachePadded<AtomicUsize>,
    buffer: LocalListBuffer<P>,
}

impl<P: Platform> Default for LocalList<P> {
    fn default() -> Self {
        Self::new()
    }
}

impl<P: Platform> LocalList<P> {
    const SIZE: usize = 128;

    pub fn new() -> Self {
        Self {
            pos: CachePadded::new(AtomicUsize::new(0)),
            buffer: LocalListBuffer::new(),
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

    pub unsafe fn push(
        &self,
        task: NonNull<Task<P>>,
        overflow: Option<(usize, NonZeroUsize, &LocalListBuffer<P>)>,
    ) -> Option<NonZeroUsize> {
        let (_, priority) = Task::decode(task);
        let is_fifo = match priority {
            TaskPriority::Low | TaskPriority::Normal => true,
            _ => false,
        };

        let mut pos = self.pos.load(Ordering::Relaxed);
        let [mut head, tail] = Self::decode(pos);

        loop {
            if tail.wrapping_sub(head) < LocalListBuffer::<P>::SIZE {
                if is_fifo {
                    let new_tail = tail.wrapping_add(1);
                    self.buffer.wrapping_write(tail, task);
                    self.tail().store(new_tail, Ordering::Release);
                    return;
                } else {
                    let new_head = head.wrapping_sub(1);
                    self.buffer.wrapping_write(new_head, task);
                    match self.pos.compare_exchange_weak(
                        pos,
                        Self::encode([new_head, tail]),
                        Ordering::Release,
                        Ordering::Relaxed,
                    ) {
                        Ok(_) => return,
                        Err(e) => pos = e,
                    }
                    continue;
                }
            }

            // TODO: overflow
        }
    }
}