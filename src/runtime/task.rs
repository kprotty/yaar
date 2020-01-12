use super::executor::{with_executor};
use core::{
    pin::Pin,
    marker::Unpin,
    future::Future,
    ptr::{null, NonNull},
    hint::unreachable_unchecked,
    mem::{align_of, MaybeUninit},
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

pub struct FutureTask<F: Future> {
    future: F,
    pub task: Task,
    output: Option<F::Output>,
}

impl<F: Future> Unpin for FutureTask<F> {}

impl<F: Future> Future for FutureTask<F> {
    type Output = NonNull<F::Output>;

    fn poll(self: Pin<&mut Self>, ctx: &mut Context<'_>) -> Poll<Self::Output> {
        let this = self.get_mut();
        loop {
            if let Some(output) = this.output.as_mut() {
                return Poll::Ready(unsafe { NonNull::new_unchecked(output) });
            }
            let pinned = unsafe { Pin::new_unchecked(&mut this.future) };
            match pinned.poll(ctx) {
                Poll::Pending => return Poll::Pending,
                Poll::Ready(output) => {
                    this.output = Some(output);
                    continue;
                },
            }
        }
    }
}

impl<F: Future> FutureTask<F> {
    pub fn new(priority: Priority, future: F) -> Self {
        Self {
            future,
            task: Task::new(priority, Self::on_resume),
            output: None,
        }
    }

    pub fn into_output(self) -> Option<F::Output> {
        self.output
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

    pub fn resume(&mut self) -> Poll<&F::Output> {
        const WAKE: unsafe fn(*const ()) = |ptr| {
            with_executor(|executor| unsafe {
                executor.schedule(&mut *(ptr as *mut Task))
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
        let pinned = unsafe { Pin::new_unchecked(self) };

        match pinned.poll(&mut Context::from_waker(&waker)) {
            Poll::Pending => Poll::Pending,
            Poll::Ready(ptr) => Poll::Ready(unsafe { &*ptr.as_ptr() }),
        }
    }
}

pub fn yield_now() -> impl Future {
    // TODO: yield_now(priority)

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
