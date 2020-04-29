use yaar_lock::utils::unreachable;
use super::{
    TaskEvent,
    TaskEventHandler,
    TaskHeader,
};
use core::{
    ptr,
    mem::MaybeUninit,
    sync::atomic::{Ordering, AtomicUsize},
};

const HAS_WAKER: usize = 1 << 0;

pub struct TaskFuture<P, E, F>
where
    P: Platform,
    E: TaskEventHandler,
    F: Future,
{
    state: AtomicUsize,
    header: TaskHeader<P>,
    output: MaybeUninit<F::Output>,
    future: MaybeUninit<F>,
    event_handler: MaybeUninit<E>,
    join_waker: MaybeUninit<Waker>,
}

impl<P, E, F> From<F> for TaskFuture<P, E, F> 
where
    P: Platform,
    E: TaskEventHandler + Default,
    F: Future,
{
    fn from(future: F) -> Self {
        Self::new(E::default(), future)
    }
}

impl<P, E, F> Drop for TaskFuture<P, E, F> 
where
    P: Platform,
    E: TaskEventHandler,
    F: Future,
{
    fn drop(&mut self) {
        unsafe {
            drop_in_place(self.future.as_mut_ptr());
            drop_in_place(self.event_handler.as_mut_ptr());
            
            let state = self.state.load(Ordering::Acquire);
            if (state & COMPLETED != 0) && (state & WAITING != 0) {
                self.state.store()
                drop_in_place(self.join_waker.as_ptr() as *mut Waker);
            }
        }
    }
}

impl<P, E, F> TaskFuture<P, E, F>
where
    P: Platform,
    E: TaskEventHandler,
    F: Future
{
    pub fn new(event_handler: E, future: F) -> Self {
        let mut this = Self {
            state: AtomicUsize::new(0),
            header: TaskHeader::new(Self::dispatch),
            output: MaybeUninit::uninit(),
            future: MaybeUninit::new(future),
            event_handler: MaybeUninit::new(event_handler),
            join_waker: MaybeUninit::uninit(),
        };

        let base_ptr = &this as *const _ as usize;
        let field_ptr = &this.header as *const _ as usize;
        this.header.header_offset = field_ptr - base_ptr;

        this
    }

    unsafe fn dispatch(header: &TaskHeader<E>, event: TaskEvent) {
        let field_ptr = header as *const _ as usize;
        let base_ptr = field_ptr - header.header_offset;
        let this = &*(base_ptr as *const Self);

        let event_handler = &*self.event_handler.as_ptr();
        match event {
            TaskEvent::Clone => {
                event_handler.on_waker_clone();
            },
            TaskEvent::Wake => {
                event_handler.on_waker_wake();
                this.schedule(this.header.worker.as_ref());
            },
            TaskEvent::WakeByRef => {
                event_handler.on_waker_wake_by_ref();
                this.schedule(this.header.worker.as_ref());
            },
            TaskEvent::Drop => {
                event_handler.on_waker_drop();
            },
            TaskEvent::Poll => {
                event_handler.on_task_poll();

                let waker = this.header.waker();
                let ctx = &mut Context::from_waker(&waker);
                let future = this.future.as_ptr() as *mut F;

                loop {
                    match Pin::new_unchecked(future).poll(ctx) {
                        Poll::Pending => {
                            if self.try_stop_running() {
                                return;
                            }
                        },
                        Poll::Ready(output) => {
                            let output_ptr = this.output.as_ptr() as *mut F::Output;
                            ptr::write(output_ptr, output);
                            self.complete();
                            return;
                        },
                    }
                }
            },
        }
    }

    fn schedule(&self, worker: &Worker<P>) {
        let mut state = self.state.load(Ordering::Relaxed);
        loop {
            let new_state = if state & RUNNING == 0 {
                state | RUNNING
            } else if state & NOTIFIED == 0 {
                state | NOTIFIED
            } else {
                return;
            };

            match self.state.compare_exchange_weak(
                state,
                new_state,
                Ordering::Relaxed,
                Ordering::Relaxed,
            ) {
                Err(e) => state = e,
                Ok(_) => return unsafe {
                    if state & RUNNING == 0 {
                        let task = unsafe { &mut *self.header.task.get() };
                        let task = Pin::new_unchecked(task);
                        let task_list = TaskList::from(task);
                        worker.schedule(task_list);
                    }
                },
            }
        }
    }

    fn try_stop_running(&self) -> bool {
        let mut state = self.state.load(Ordering::Relaxed);
        loop {
            let new_state = if state & NOTIFIED != 0 {
                state & !RUNNING & !NOTIFIED
            } else {
                state & !RUNNING
            };

            match self.state.compare_exchange_weak(
                state,
                new_state,
                Ordering::Relaxed,
                Ordering::Relaxed,
            ) {
                Err(e) => state = e,
                Ok(_) => return state & NOTIFIED == 0,
            }
        }
    }

    unsafe fn wait_for_output(&self, waker: &Waker) -> Poll<F::Output> {
        let mut state = self.state.load(Ordering::Acquire);
        loop {
            if state & COMPLETED != 0 {
                return Poll::Ready(ptr::read(self.output.as_ptr()));
            }

            let new_state = (state & !WAITING) | UPDATING;
            if let Err(e) = self.state.compare_exchange_weak(
                state,
                new_state,
                Ordering::Acquire,
                Ordering::Acquire,
            ) {
                state = e;
                continue;
            }

            let waker_ptr = self.join_waker.as_ptr() as *mut Waker;
            if state & WAITING != 0 {
                drop_in_place(waker_ptr);
            }
            ptr::write(waker_ptr, waker.clone());
            state = new_state;

            loop {
                match self.state.compare_exchange_weak(
                    state,
                    (state & !UPDATING) | WAITING,
                    Ordering::AcqRel,
                    Ordering::Acquire,
                ) {
                    Ok(_) => return,
                    Err(e) => {
                        state = e;
                        if state & COMPLETED != 0 {
                            drop_in_place(waker_ptr);
                            return Poll::Ready(ptr::read(self.output.as_ptr()));
                        }
                    },
                }
            }
        }
    }

    fn complete(&self) {
        let mut state = self.state.load(Ordering::Relaxed);
        loop {
            match self.state.compare_exchange_weak(
                state,
                (state & (WAITING | UPDATING)) | COMPLETED,
                Ordering::Acquire,
                Ordering::Relaxed,
            ) {
                Err(e) => state = e,
                Ok(_) => return unsafe {
                    if state & WAITING != 0 {
                        let waker_ptr = self.join_waker.as_ptr() as *mut Waker;
                        (&*waker_ptr).wake_by_ref();
                        drop_in_place(waker_ptr);
                    }
                },
            }            
        }
    }
}