use core::{
    pin::Pin,
    future::Future,
    cell::UnsafeCell,
    ptr::{null, NonNull},
    hint::unreachable_unchecked,
    mem::{self, align_of, MaybeUninit},
    task::{Poll, Context, Waker, RawWaker, RawWakerVTable},
};

pub enum Priority {
    Low = 0,
    Normal = 1,
    High = 2,
}

impl Priority {
    const MASK: usize = 3;
}

pub struct Task {
    next: usize,
    resume: unsafe fn(*mut Task),
}

impl Task {
    pub fn new(priority: Priority, resume: unsafe fn(*mut Task)) -> Self {
        assert!(align_of::<Self>() > Priority::MASK);
        let next = priority as usize;
        Self { next, resume }
    }

    pub fn get_next(&self) -> Option<NonNull<Self>> {
        NonNull::new((self.next & !Priority::MASK) as *mut Self)
    }

    pub fn set_next(&mut self, next: Option<NonNull<Self>>) {
        self.next = (self.next & Priority::MASK) | match next {
            Some(ptr) => ptr.as_ptr() as usize,
            None => null::<Self>() as usize,
        };
    }

    pub fn set_priority(&mut self, priority: Priority) {
        self.next = (self.next & !Priority::MASK) | (priority as usize);
    }

    pub fn get_priority(&self) -> Priority {
        match self.next & Priority::MASK {
            0 => Priority::Low,
            1 => Priority::Normal,
            2 => Priority::High,
            _ => unsafe { unreachable_unchecked() }
        }
    }

    pub fn resume(&mut self) {
        unsafe { (self.resume)(self) }
    }
}

#[doc(hidden)]
#[derive(Default)]
pub struct List {
    head: Option<NonNull<Task>>,
    tail: Option<NonNull<Task>>,
}

impl List {
    pub fn pop<'a>(&mut self) -> Option<&'a mut Task> {
        self.head.map(|task| {
            let task = unsafe { &mut *task.as_ptr() };
            self.head = task.get_next();
            if self.head.is_none() {
                self.tail = None;
            }
            task
        })
    }

    pub fn push(&mut self, priority_list: &PriorityList) {
        self.push_front(&priority_list.front);
        self.push_back(&priority_list.back);
    }

    pub fn push_front(&mut self, list: &Self) {
        if let Some(tail) = list.tail {
            unsafe { (&mut *tail.as_ptr()).set_next(self.head) };
        }
        if self.tail.is_none() {
            self.tail = list.tail;
        }
        self.head = list.head;
    }

    pub fn push_back(&mut self, list: &Self) {
        if let Some(tail) = self.tail {
            unsafe { (&mut *tail.as_ptr()).set_next(list.head) };
        }
        if self.head.is_none() {
            self.head = list.head;
        }
        self.tail = list.tail;
    }
}

#[doc(hidden)]
#[derive(Default)]
pub struct PriorityList {
    front: List,
    back: List,
    pub size: usize,
}

impl PriorityList {
    pub fn pop<'a>(&mut self) -> Option<&'a mut Task> {
        self.front.pop()
            .or_else(|| self.back.pop())
            .map(|task| {
                self.size -= 1;
                task
            })
    }

    pub fn push(&mut self, task: &mut Task) {
        self.size += 1;
        let list = List {
            head: NonNull::new(task),
            tail: NonNull::new(task),
        };
        match task.get_priority() {
            Priority::Low | Priority::Normal => self.back.push_back(&list),
            Priority::High => self.front.push_back(&list),
        }
    }
}

pub trait Executor: Sync {
    fn schedule(&self, task: &mut Task);
}

struct ExecutorVTable {
    schedule: unsafe fn(*const (), task: *mut Task),
}

struct ExecutorRef(UnsafeCell<Option<(*const (), &'static ExecutorVTable)>>);

unsafe impl Sync for ExecutorRef {}

/// Global reference to the current executor implementation.
/// Only modified by with_executor_as() which should not be called
/// via multiple threads to its safe to use a mutable global.
static EXECUTOR_REF: ExecutorRef = ExecutorRef(UnsafeCell::new(None));

pub fn with_executor_as<E: Executor, T>(executor: &E, scoped: impl FnOnce(&E) -> T) -> T {
    // would have been const but "can't use generic parameters from outer function"
    let vtable = ExecutorVTable {
        schedule: |ptr, task| unsafe {
            (&*(ptr as *const E)).schedule(&mut *task)
        },
    };

    unsafe {
        // promote our local vtable to static so it can be accessed from a global setting
        let vtable_ref = &*(&vtable as *const _);
        let old_executor = mem::replace(
            &mut *EXECUTOR_REF.0.get(),
            Some((executor as *const E as *const (), vtable_ref)),
        );

        let result = scoped(executor);

        // restore the old executor and make sure ours was on-top of the stack
        let (our_ptr, our_vtable) = mem::replace(&mut *EXECUTOR_REF.0.get(), old_executor).unwrap();
        debug_assert_eq!(our_ptr as usize, executor as *const _ as usize);
        debug_assert_eq!(our_vtable as *const _ as usize, vtable_ref as *const _ as usize);

        result
    }
}

fn with_executor<T>(scoped: impl FnOnce(&(*const (), &'static ExecutorVTable)) -> T) -> T {
    scoped(unsafe {
        (&*EXECUTOR_REF.0.get())
            .as_ref()
            .expect("Executor is not set. Make sure this is invoked only inside the scope of `with_executor_as()`")
    })
}

pub struct FutureTask<F: Future> {
    future: F,
    pub task: Task,
    result: Option<F::Output>,
}

impl<F: Future> FutureTask<F> {
    pub fn new(priority: Priority, future: F) -> Self {
        Self {
            future,
            task: Task::new(priority, Self::on_resume),
            result: None,
        }
    }

    pub fn into_inner(self) -> Option<F::Output> {
        self.result
    }

    unsafe fn on_resume(task: *mut Task) {
        // zig: @fieldParentPtr(Self, "task", task)
        let this = {
            let stub = MaybeUninit::<Self>::zeroed();
            let base_ptr = stub.as_ptr() as usize;
            let task_ptr = &(&*stub.as_ptr()).task as *const _ as usize;
            &mut *(((task as usize) - (task_ptr - base_ptr)) as *mut Self)
        };
        let _ = this.resume();
    }

    pub fn resume(&mut self) -> Poll<Option<&F::Output>> {
        if self.result.is_some() {
            return Poll::Ready(self.result.as_ref());
        }

        const WAKE: unsafe fn(*const ()) = |ptr| {
            with_executor(|&(executor, vtable)| unsafe {
                (vtable.schedule)(executor, ptr as *mut Task)
            })
        };

        const VTABLE: RawWakerVTable = RawWakerVTable::new(
            |ptr| RawWaker::new(ptr, &VTABLE),
            WAKE,
            WAKE,
            |_| {},
        );

        let ptr = &self.task as *const Task as *const ();
        let waker = unsafe { Waker::from_raw(RawWaker::new(ptr, &VTABLE)) };
        let pinned = unsafe { Pin::new_unchecked(&mut self.future) };
        let mut context = Context::from_waker(&waker);

        match pinned.poll(&mut context) {
            Poll::Pending => Poll::Pending,
            Poll::Ready(result) => {
                self.result = Some(result);
                Poll::Ready(self.result.as_ref())
            }
        }
    }
}

pub fn yield_now() -> impl Future {
    struct YieldTask {
        did_yield: bool,
    };

    impl Future for YieldTask {
        type Output = ();

        fn poll(mut self: Pin<&mut Self>, ctx: &mut Context<'_>) -> Poll<Self::Output> {
            if self.did_yield {
                return Poll::Ready(());
            }

            self.did_yield = true;
            ctx.waker().wake_by_ref();
            Poll::Pending
        }
    }

    YieldTask {
        did_yield: false,
    }
}
