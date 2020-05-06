use crate::executor::{
    Platform, Worker,
    task::{Task, TaskResumeFn},
};
use core::{
    pin::Pin,
    future::Future,
    ptr::{self, NonNull},
    mem::MaybeUninit,
    marker::PhantomData,
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
pub struct TaskFuture<P: Platform, F: Future, W: TaskWaker> {
    task: Task<P>,
    task_waker: W,
    future_state: FutureState<F>,
}

impl<P: Platform, F: Future, W: TaskWaker> Drop for TaskFuture<P, F, W> {
    fn drop(&mut self) {

    }
}

impl<P: Platform, F: Future, W: TaskWaker> TaskFuture<P, F, W> {
    const VTABLE: TaskVTable<P> = TaskVTable {
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

    unsafe fn waker_clone(task: Pin<&Task<P>>) {

    }

    unsafe fn waker_wake(task: Pin<&Task<P>>) {
        
    }

    unsafe fn waker_wake_by_ref(task: Pin<&Task<P>>) {
        
    }

    unsafe fn waker_drop(task: Pin<&Task<P>>) {
        
    }

    unsafe fn task_resume(task: Pin<&Task<P>>) {
        
    }
}

#[repr(C)]
struct TaskVTable<P: Platform> {
    resume: TaskResumeFn,
    clone: unsafe fn(task: NonNull<Task<P>>),
    wake: unsafe fn(task: NonNull<Task<P>>),
    wake_by_ref: unsafe fn(task: NonNull<Task<P>>),
    drop: unsafe fn(task: NonNull<Task<P>>),
}

#[repr(C)]
pub(crate) struct TaskPollContext<P: Platform> {
    waker: Waker,
    worker: NonNull<Worker<P>>,
    next_task: Option<NonNull<Task<P>>>,
}

impl<P: Platform> TaskPollContext<P> {
    const VTABLE: RawWakerVTable = RawWakerVTable::new(
        |ptr| unsafe {
            let vtable = *(ptr as *const Task<P>).resume.cast::<TaskVTable<P>>();
            (vtable.as_ref().clone)(NonNull::new_unchecked(ptr as *mut Task<P>));
            RawWaker::new(ptr, &Self::VTABLE)
        },
        |ptr| unsafe {
            let vtable = *(ptr as *const Task<P>).resume.cast::<TaskVTable<P>>();
            (vtable.as_ref().wake)(NonNull::new_unchecked(ptr as *mut Task<P>));
        },
        |ptr| unsafe {
            let vtable = *(ptr as *const Task<P>).resume.cast::<TaskVTable<P>>();
            (vtable.as_ref().wake_by_ref)(NonNull::new_unchecked(ptr as *mut Task<P>));
        },
        |ptr| unsafe {
            let vtable = *(ptr as *const Task<P>).resume.cast::<TaskVTable<P>>();
            (vtable.as_ref().drop)(NonNull::new_unchecked(ptr as *mut Task<P>));
        },
    );

    unsafe fn poll<F: Future>(
        &mut self,
        future: &mut F,
        task: NonNull<Task<P>>,
        worker: NonNull<Worker<P>>,
    ) -> Result<(Poll<F::Output>, Option<Pin<&mut Task<P>>>), TaskFutureError> {
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

    pub unsafe fn waker_to_task(waker: &Waker) -> Option<NonNull<Task<P>>> {
        assert_eq!(mem::size_of::<Waker>(), mem::size_of::<[usize; 2]>());
        let vtable_ptr = &Self::VTABLE as *const _ as usize;
        let raw_waker: [usize; 2] = ptr::read(waker as *const _ as *const _);

        if raw_waker[0] == vtable_ptr {
            Some(NonNull::new_unchecked(raw_waker[1] as *mut Task<P>))
        } else if raw_waker[1] == vtable_ptr {
            Some(NonNull::new_unchecked(raw_waker[0] as *mut Task<P>))
        } else {
            None
        }
    }

    pub unsafe fn try_with<T>(
        callback: impl FnOnce(&mut Self) -> T,
    ) -> impl Fuutre<Output = Option<T>> { 
        struct TryWith<P, F, T>
        where
            P: Platform,
            F: FnOnce(&mut TaskPollContext<P>) -> T
        {
            callback: Option<F>,
        }

        impl<P, F, T> Future for TryWith<P, F, T>
        where
            P: Platform,
            F: FnOnce(&mut TaskPollContext<P>) -> T,
        {
            type Output = Option<T>;

            fn poll(self: Pin<&mut Self>, ctx: &mut Context<'_>) -> Poll<Self::Output> {
                let callback = self
                    .callback
                    .take()
                    .expect("try_with() polled after completion");

                Poll::Ready(unsafe {
                    Self::waker_to_task(ctx.waker()).map(|_| {
                        let poll_context_ptr = ctx.waker() as *const _ as *mut TaskPollContext<P>;
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

pub fn try_with_worker<P, F, T>(
    callback: F,
) -> impl Future<Output = Option<T>> 
where
    P: Platform,
    F: FnOnce(&Worker<P>) -> T,
{
    struct TryWithWorker<P, F, T>
    where
        P: Platform,
        F: FnOnce(&Worker<P>) -> T,
    {
        callback: Option<F>,
    }

    impl<P, F, T> Future for TryWithWorker<P, F, T>
    where
        P: Platform,
        F: FnOnce(&Worker<P>) -> T,
    {
        type Output = Option<T>;

        fn poll(self: Pin<&mut Self>, ctx: &mut Context<'_>) -> Poll<Self::Output> {
            let callback = self
                .callback
                .take()
                .expect("try_with_worker() polled after completion");

            let mut try_with = TaskPollContext::<P>::try_with(|poll_context| {
                callback(unsafe { poll_context.worker.as_ref() })
            });

            Pin::new(&mut try_with).poll(ctx)
        }
    }

    TryWithWorker {
        callback: Some(callback),
    }
}

pub fn try_yield_to<P: Platform>(
    waker: Waker,
) -> impl Future<Output = Result<(), Waker>> {
    struct TryYieldTo<P: Platform> {
        waker: Option<Waker>,
        _phantom: PhantomData<*mut P>,
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
                TaskPollContext::<P>::waker_to_task(&task)
                    .map(|task| TaskPollContext::<P>::try_with(|poll_ctx| {
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
        _phantom: PhantomData,
    }
}