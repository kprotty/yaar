use super::{
    Task,
    TaskResumeFn,
    super::Worker,
};
use core::{
    pin::Pin,
    future::Future,
    ptr::{self, NonNull},
    mem::MaybeUninit,
    task::{Waker, Context, Poll, RawWaker, RawWakerVTable},
    sync::atomic::{Ordering, AtomicUsize},
};

pub unsafe trait TaskWaker {
    fn on_clone(&self, clone_fn: impl FnOnce());
    fn on_wake(&self, wake_fn: impl FnOnce());
    fn on_wake_by_ref(&self, wake_by_ref_fn: impl FnOnce());
    fn on_drop(&self, drop_fn: impl FnOnce());
    fn on_poll(&self, poll_fn: impl FnOnce());
}

#[cfg(not(feature = "std"))]
type TaskFutureError = ();

#[cfg(feature = "std")]
type TaskFutureError = Box<dyn std::any::Any + Sized + 'static>;

#[repr(C, usize)]
enum FutureState<F: Future> {
    Pending(F),
    Ready(F::Output),
    Error(TaskFutureError),
}

#[repr(C)]
pub struct TaskFuture<F: Future, W: TaskWaker> {
    task: Task,
    task_waker: W,
    future_state: FutureState,
}

impl<F: Future, W: TaskWaker> Drop for TaskFuture<F, W> {
    fn drop(&mut self) {
        unimplemented!(); // TODO
    }
}

impl<F: Future, W: TaskWaker> TaskFuture<F, W> {
    const VTABLE: TaskVTable = TaskVTable {
        clone: Self::waker_clone,
        wake: Self::waker_wake,
        wake_by_ref: Self::waker_wake_by_ref,
        drop: Self::waker_drop,
        resume: Self::task_resume,
    };

    pub fn new(task_waker: W, future: F) {
        Self {
            task: Task::new(NonNull::from(&Self::VTABLE.resume)),
            task_waker,
            future_state: FutureState::Pending(future),
        }
    }

    unsafe fn waker_clone(task: Pin<&Task>) {

    }

    unsafe fn waker_wake(task: Pin<&Task>) {
        
    }

    unsafe fn waker_wake_by_ref(task: Pin<&Task>) {
        
    }

    unsafe fn waker_drop(task: Pin<&Task>) {
        
    }

    unsafe fn task_resume(task: Pin<&Task>) {
        
    }
}

#[repr(C)]
struct TaskVTable {
    resume: TaskResumeFn,
    clone: unsafe fn(task: NonNull<Task>),
    wake: unsafe fn(task: NonNull<Task>),
    wake_by_ref: unsafe fn(task: NonNull<Task>),
    drop: unsafe fn(task: NonNull<Task>),
}

#[repr(C)]
pub(crate) struct TaskPollContext {
    waker: Waker,
    worker: NonNull<Worker>,
    next_task: Option<NonNull<Task>>,
}

impl TaskPollContext {
    const VTABLE: RawWakerVTable = RawWakerVTable::new(
        |ptr| unsafe {
            let vtable = (*(ptr as *const Task)).resume.cast::<TaskVTable>();
            (vtable.as_ref().clone)(NonNull::new_unchecked(ptr as *mut Task));
            RawWaker::new(ptr, &Self::VTABLE)
        },
        |ptr| unsafe {
            let vtable = (*(ptr as *const Task)).resume.cast::<TaskVTable>();
            (vtable.as_ref().wake)(NonNull::new_unchecked(ptr as *mut Task));
        },
        |ptr| unsafe {
            let vtable = (*(ptr as *const Task)).resume.cast::<TaskVTable>();
            (vtable.as_ref().wake_by_ref)(NonNull::new_unchecked(ptr as *mut Task));
        },
        |ptr| unsafe {
            let vtable = (*(ptr as *const Task)).resume.cast::<TaskVTable>();
            (vtable.as_ref().drop)(NonNull::new_unchecked(ptr as *mut Task));
        },
    );

    unsafe fn poll<F: Future>(
        &mut self,
        future: &mut F,
        task: NonNull<Task>,
        worker: NonNull<Worker>,
    ) -> Result<(Poll<F::Output>, Option<Pin<&mut Task>>), TaskFutureError> {
        let mut poll_context = Self {
            waker: Waker::from_raw(RawWaker::new(
                task.as_ptr() as *const (),
                &Self::VTABLE,
            )),
            worker,
            next_task: None,
        };
        
        let future = Pin::new_unchecked(future);
        let mut context = Context::from_waker(&poll_context);

        // Poll the future either normally or to catch unwind panics under std.
        // The, consume and retyrn any `next_task` that was stored during the poll.
        {
            #[cfg(not(feature = "std"))] {
                Ok(future.poll(&mut context))
            }
            #[cfg(feature = "std")] {
                use std::panic::{catch_unwind, AssertUnwind};
                catch_unwind(AssertUnwind(|| future.poll(&mut context)))
            }
        }.map(|poll_result| {
            let next_task = poll_context
                .next_task
                .take()
                .map(|ptr| &mut *ptr.as_ptr())
                .map(Pin::new_unchecked);
            (poll_result, next_task)
        })
    }

    pub unsafe fn waker_to_task(waker: &Waker) -> Option<NonNull<Task>> {
        assert_eq!(mem::size_of::<Waker>(), mem::size_of::<[usize; 2]>());
        let vtable_ptr = &Self::VTABLE as *const _ as usize;
        let raw_waker: [usize; 2] = ptr::read(waker as *const _ as *const _);

        if raw_waker[0] == vtable_ptr {
            Some(NonNull::new_unchecked(raw_waker[1] as *mut Task))
        } else if raw_waker[1] == vtable_ptr {
            Some(NonNull::new_unchecked(raw_waker[0] as *mut Task))
        } else {
            None
        }
    }

    pub unsafe fn try_with<T>(
        callback: impl FnOnce(&mut Self) -> T,
    ) -> impl Fuutre<Output = Option<T>> { 
        struct TryWith<F, T> where F: FnOnce(&mut TaskPollContext<P>) -> T {
            callback: Option<F>,
        }

        impl<F, T> Future for TryWith<F, T> where F: FnOnce(&mut TaskPollContext<P>) -> T{
            type Output = Option<T>;

            fn poll(self: Pin<&mut Self>, ctx: &mut Context<'_>) -> Poll<Self::Output> {
                let callback = self
                    .callback
                    .take()
                    .expect("try_with() polled after completion");

                Poll::Ready(unsafe {
                    Self::waker_to_task(ctx.waker()).map(|_| {
                        let poll_context_ptr = ctx.waker() as *const _ as *mut TaskPollContext;
                        callback(NonNull::new_unchecked(poll_context_ptr).as_mut())
                    })
                })
            }
        }

        TryWith {
            callback: Some(callback),
        }
    }
}

pub fn try_with_worker<F, T>(
    callback: F,
) -> impl Future<Output = Option<T>> 
where
    F: FnOnce(&Worker<P>) -> T
{
    struct TryWithWorker<F, T>
    where
        F: FnOnce(&Worker<P>) -> T,
    {
        callback: Option<F>,
    }

    impl<F, T> Future for TryWithWorker<F, T>
    where
        F: FnOnce(&Worker<P>) -> T,
    {
        type Output = Option<T>;

        fn poll(self: Pin<&mut Self>, ctx: &mut Context<'_>) -> Poll<Self::Output> {
            let callback = self
                .callback
                .take()
                .expect("try_with_worker() polled after completion");

            let mut try_with = TaskPollContext::try_with(|poll_context| {
                callback(unsafe { poll_context.worker.as_ref() })
            });

            Pin::new(&mut try_with).poll(ctx)
        }
    }

    TryWithWorker {
        callback: Some(callback),
    }
}

pub fn try_yield_to(
    waker: Waker,
) -> impl Future<Output = Result<(), Waker>> {
    struct TryYieldTo {
        waker: Option<Waker>,
    }

    impl<P: Platform> Future for TryYieldTo<P> {
        type Output = Result<(), Waker>;

        fn poll(self: Pin<&mut Self>, ctx: &mut Context<'_>) -> Poll<Self::Output> {
            let waker = self
                .waker
                .take()
                .expect("try_yield_to() polled after completion");

            let mut waker = Some(waker);
            Poll::Ready(unsafe {
                TaskPollContext::waker_to_task(&task)
                    .map(|task| TaskPollContext::try_with(|poll_ctx| {
                        poll_ctx.next_task = Some(task);
                        mem::drop(waker.take());
                    }))
                    .and_then(|mut try_with| match Pin::new(&mut try_with).poll(ctx) {
                        Poll::Pending => unreachable(),
                        Poll::Ready(result) => result,
                    })
                    .ok_or_else(|| {
                        waker.take().unwrap_unchecked()
                    })
            })
        }
    }

    TryYieldTo {
        waker: Some(waker),
    }
}