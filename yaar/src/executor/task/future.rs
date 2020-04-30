use super::{
    super::{Platform, Worker},
    Task,
    TaskWaker,
    TaskResumeFn,
};

use core::{
    mem::{self, MaybeUninit},
    ptr::{self, NonNull},
};

pub struct TaskFuture<P, W, F>
where
    P: Platform,
    W: TaskWaker,
    F: Future,
{
    task: Task<P>,
    waker: MaybeUninit<W>,
    future: MaybeUninit<F>,
    join_handle: TaskJoinHandle<F::Output>,
}

impl<P, W, F> Drop for TaskFuture<P, W, F>
where
    P: Platform,
    W: TaskWaker,
    F: Future,
{
    fn drop(&mut self) {
        unsafe {
            ptr::drop_in_place(self.future.as_mut_ptr());
            ptr::drop_in_place(self.waker.as_mut_ptr());
        }
    }
}

impl<P, W, F> From<F> for TaskFuture<P, W, F>
where
    P: Platform,
    W: TaskWaker + Default,
    F: Future,
{
    fn from(future: F) -> Self {
        Self::new(W::default(), future)
    }
}

impl<P, W, F> TaskFuture<P, W, F>
where
    P: Platform,
    W: TaskWaker,
    F: Future,
{
    pub fn new(waker: W, future: F) -> Self {
        Self {
            task: Task::new(Self::resume),
            waker: MaybeUninit::new(waker),
            future: MaybeUninit::new(future),
            join_handle: TaskJoinHandle::new(),
        }
    }

    fn resume(task: &mut Task<P>, worker: Pin<&Worker<P>>) {
        let task_offset = {
            let stub = Self {
                task: Task::new(Self::resume),
                waker: MaybeUninit::uninit(),
                future: MaybeUninit::uninit(),
                join_handle: TaskJoinHandle::new(),
            };

            let base_ptr = &stub as *const _ as usize;
            let task_ptr = &stub.task as *const _ as usize;
            mem::forget(stub);
            task_ptr - base_ptr
        };

        let task_ptr = task as *mut _ as usize;
        let this_ptr = (task_ptr - task_offset) as *const Self;
        
        unsafe {
            let worker = worker.into_inner_unchecked();
            loop {
                let future = &mut *((*this_ptr).future.as_ptr() as *mut _);
                let future = Pin::new_unchecked(future);
                
                // TODO:
                // - store joining future ptr in state
                // - support join detaching
                // - pub TaskFuture::schedule(hints) -> state | NOTIFIED -> schedule if !(state & RUNNING) 
                //      - hints: Priority, Locality, etc.
                // - store P::TaskLocal in TaskFuture or Task
                // - store Option<NonNull<Worker>> in TaskFuture to use when waking
                // - PollContext{NonNull<TaskLocal>, Context}
                // - with_poll_context(ctx, f) -> check waker's vtable, compute ^ from ctx using NonNull::dangling() for other fields
                //      - with_task_local_ref(|ref| t) -> Option<T>

                // TODO:
                // - OsThreadPool:
                //   - builder enum:
                //      - SMP: max_threads
                //      - NUMA: &[node_ids},
                //      - Custom: (&[Nodes])
                //   - with_timer
                //   - with_io
                // - spawn() -> .detach() | .join()
                // Self::run(future) for simple startup

                let retry = match TaskPollContext::poll(future, worker) {
                    Poll::Pending => {
                        (*this_ptr).join_handle.poll_pending()
                    },
                    Poll::Ready(output) => {
                        (*this_ptr).join_handle.poll_ready(output); 
                    },
                }
            }
        }
    }
}