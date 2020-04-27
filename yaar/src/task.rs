use core::{
    cell::UnsafeCell,
    num::NonZeroUsize,
    ptr::NonNull,
    pin::Pin,
    hint::unreachable_unchecked,
    marker::PhantomPinned,
    mem::{align_of, transmute, MaybeUninit, replace},
    sync::atomic::{spin_loop_hint, Ordering, AtomicUsize, AtomicPtr, AtomicBool},
};

pub trait Platform {

}

pub enum RunError {
    EmptyNodes,
    EmptyWorkersOnNode(usize),
}

pub struct Scheduler<P: Platform> {
    platform: NonNull<P>,
    nodes_ptr: NonNull<NonNull<Node<P>>>,
    nodes_len: NonZeroUsize,
}

unsafe impl<P: Platform> Sync for Scheduler<P> {}

impl<P: Platform> Scheduler<P> {
    pub unsafe fn run(
        task: NonNull<Task>,
        platform: &P,
        nodes: &[NonNull<Node<P>>],
    ) -> Result<(), RunError> {
        let scheduler = Self {
            platform: NonNull::from(platform),
            nodes_ptr: NonNull::new_unchecked(nodes.as_ptr() as *mut _),
            nodes_len: match NonZeroUsize::new(nodes.len()) {
                None => return Err(RunError::EmptyNodes),
                Some(len) => len,
            },
        };


    }

    pub fn schedule(&self, list: &TaskList) {

    }
}

pub struct Node<P: Platform> {
    scheduler: NonNull<P>,
    workers_ptr: NonNull<NonNull<Worker<P>>>,
    workers_len: NonZeroUsize,
}

pub struct Worker<P: Platform> {
    node: NonNull<Node<P>>,
    
}

struct GlobalList {
    _pin: PhantomPinned,
    tail: AtomicPtr<Task>,
    is_locked: AtomicBool,
    list: UnsafeCell<TaskList>,
    stub: Task,
}

impl GlobalList {
    fn init(self: Pin<&mut Self>) {
        let mut_self = unsafe { self.get_unchecked_mut() };
        *mut_self = Self {
            _pin: PhantomPinned,
            tail: AtomicPtr::new(&mut_self.stub as *const _ as *mut _),
            is_locked: AtomicBool::new(false),
            list: UnsafeCell::new(TaskList::default()),
            stub: Task::new(
                Locality::Worker,
                Priority::Low,
                |_| unreachable!(),
            ),
        };
    }

    #[inline]
    fn try_with_list<T>(
        &self,
        f: impl FnOnce(&mut TaskList) -> Option<T>,
    ) -> Option<T> {
        if !self.is_locked.load(Ordering::Relaxed) {
            if !self.is_locked.compare_and_swap(false, true, Ordering::Acquire) {
                let result = f(unsafe { &mut *self.list.get() });
                self.is_locked.store(false, Ordering::Release);
                return result;
            }
        }

        None
    }

    #[inline]
    unsafe fn push(&self, list: &TaskList) {
        if list.head.is_none() {
            return;
        }

        let front = list.head.unwrap_or_else(|| unreachable_unchecked());
        let back = list.tail.unwrap_or_else(|| unreachable_unchecked());

        let prev = self.tail.swap(back.as_ptr(), Ordering::AcqRel);
        let next_tag = prev.as_ref().next & 0b11;
        let next_ref = &*(&prev.as_ref().next as *const _ as *const AtomicUsize);
        next_ref..store(next_tag | (front.as_ptr() as usize), Ordering::Release);
    }

    unsafe fn pop(&self, local_list: &LocalList) -> Option<NonNull<Task>> {
        self.try_with_list(|list| {
            let tail = *(&local_list.tail as *const _ as *const usize);
            let head = local_list.head.load(Ordering::Relaxed);
            let mut first_task = None;

            for i in 0..(tail.wrapping_sub(head) + 1) {
                // TODO: impl global queue alg from worker.rs
            }
        })
    }
}

struct LocalList {
    head: AtomicUsize,
    tail: AtomicUsize,
    buffer: MaybeUninit<[NonNull<Task>; Self::SIZE]>,
}

impl LocalList {
    const SIZE: usize = 256;

    fn init(&mut self) {
        *self.head.get_mut() = 0;
        *self.tail.get_mut() = 0;
        self.buffer = unsafe { MaybeUninit::uninit() };
    }

    unsafe fn pop(&self) -> Option<NonNull<Task>> {
        let tail = *(&self.tail as *const _ as *const usize);
        let mut head = self.head.load(Ordering::Relaxed);
        let buffer = &*self.buffer.as_ptr();

        while tail.wrapping_sub(head) != 0 {
            let task = *buffer.get_unchecked(head % Self::SIZE);
            match self.head.compare_exchange_weak(
                head,
                head.wrapping_add(1),
                Ordering::Relaxed,
                Ordering::Relaxed,
            ) {
                Ok(_) => return Some(task),
                Err(e) => head = e,
            }
        }

        None
    }

    unsafe fn steal_from(&self, target: &LocalList) -> Option<NonNull<Task>> {
        let buffer = self.buffer.as_ptr() as *mut NonNull<Task>;
        let tail = *(&self.tail as *const _ as *const usize);
        let head = self.head.load(Ordering::Relaxed);
        let size = tail.wrapping_sub(head);

        loop {
            let target_head = target.head.load(Ordering::Acquire);
            let target_tail = target.tail.load(Ordering::Acquire);

            let steal_size = target_tail.wrapping_sub(target_head);
            let steal_size = NonZeroUsize::new_unchecked(match steal_size {
                0 => return None,
                1 => 1,
                target_size => (target_size >> 1).min(size),
            });

            for i in 0..steal_size.get() {
                let index = target_head.wrapping_add(i) % Self::SIZE;
                let task = *target.buffer.as_ptr().get_unchecked(index);
                *buffer.add(tail.wrapping_add(i) % Self::SIZE) = task;
            }

            if let Err(_) = target.head.compare_exchange_weak(
                target_head,
                target_head.wrapping_add(steal_size.get()),
                Ordering::Relaxed,
                Ordering::Relaxed,
            ) {
                spin_loop_hint();
                continue;
            }

            let size = steal_size.get() - 1;
            if let Some(size) = NonZeroUsize::new(size) {
                self.tail.store(tail.wrapping_add(size), Ordering::Release);
            }
            return Some(*buffer.add(size));
        }
    }
}

#[derive(Default, Debug)]
struct TaskList {
    head: Option<NonNull<Task>>,
    tail: Option<NonNull<Task>>,
}

impl From<NonNull<Task>> for TaskList {
    fn from(task: NonNull<Task>) -> Self {
        Self {
            head: Some(task),
            tail: Some(task),
        }
    }
}

impl TaskList {
    pub unsafe fn push(&mut self, task: NonNull<Task>) {
        task.as_ref().set_next(None);
        if let Some(tail) = replace(&mut self.tail, Some(task)) {
            task.as_mut().set_next(Some(task));
        } else {
            self.head = Some(task);
        };
    }

    pub unsafe fn pop(&mut self) -> Option<NonNull<Task>> {
        self.head.map(|task| {
            self.head = task.as_ref().next();
            if self.head.is_none() {
                self.tail = None;
            }
            task
        })
    }
}

pub enum Locality {
    Worker,
    Node,
    Scheduler,
}

pub enum Priority {
    Low,
    Normal,
    High,
}

pub struct Task {
    next: usize,
    resume: usize,
}

impl Task {
    pub fn new(
        locality: Locality,
        priority: Priority,
        resume_fn: unsafe fn(NonNull<Self>),
    ) -> Self {
        assert!(align_of::<Self>() > 0b11);
        let resume_fn: usize = unsafe { transmute(resume_fn) };
        Self {
            next: match priority {
                Priority::Low => 0,
                Priority::Normal => 1,
                Priority::High => 2,
            },
            resume: resume_fn | match locality {
                Locality::Worker => 0,
                Locality::Node => 1,
                Locality::Scheduler => 2,
            },
        }
    }

    pub unsafe fn resume(&self) {
        let resume: unsafe fn(NonNull<Self>) = transmute(self.resume & !0b11);
        resume(NonNull::from(self));
    }

    pub fn priority(&self) -> Priority {
        match self.next & 0b11 {
            0 => Priority::Low,
            1 => Priority::Normal,
            2 => Priority::High,
            _ => {
                #[cfg(debug_assertions)] unreachable!();
                unsafe { unreachable_unchecked() };
            },
        }
    }

    pub fn locality(&self) -> Locality {
        match self.resume & 0b11 {
            0 => Locality::Worker,
            1 => Locality::Node,
            2 => Locality::Scheduler,
            _ => {
                #[cfg(debug_assertions)] unreachable!();
                unsafe { unreachable_unchecked() };
            },
        }
    }

    fn next(&self) -> Option<NonNull<Self>> {
        NonNull::new((self.next & !0b11) as *mut _)
    }

    fn set_next(&mut self, next: Option<NonNull<Self>>) {
        let next = next.map(|p| p.as_ptr() as usize).unwrap_or(0);
        self.next = next | (self.next & 0b11);
    }
}
