use super::pool::{Pool, PoolEvent};
use std::{
    cell::UnsafeCell,
    future::Future,
    marker::{PhantomData, PhantomPinned},
    mem::{self, MaybeUninit},
    pin::Pin,
    ptr::{self, NonNull},
    sync::{
        atomic::{AtomicPtr, AtomicU8, AtomicUsize, Ordering},
        Arc,
    },
    task::{Context, Poll, RawWaker, RawWakerVTable, Waker},
};

#[derive(Copy, Clone, Debug, Eq, PartialEq)]
enum TaskWakerState {
    Empty = 0,
    Updating = 1,
    Ready = 2,
    Notified = 3,
}

impl Into<u8> for TaskWakerState {
    fn into(self) -> u8 {
        self as u8
    }
}

impl From<u8> for TaskWakerState {
    fn from(value: u8) -> Self {
        match value {
            0 => Self::Empty,
            1 => Self::Updating,
            2 => Self::Ready,
            3 => Self::Notified,
            _ => unreachable!(),
        }
    }
}

#[derive(Default)]
struct TaskWaker {
    state: AtomicU8,
    waker: UnsafeCell<Option<Waker>>,
}

impl TaskWaker {
    fn poll(&self, waker: &Waker) -> Poll<()> {
        let state: TaskWakerState = self.state.load(Ordering::Acquire).into();
        if state == TaskWakerState::Notified {
            return Poll::Ready(());
        }

        if let Err(new_state) = self.state.compare_exchange(
            state.into(),
            TaskWakerState::Updating.into(),
            Ordering::Acquire,
            Ordering::Acquire,
        ) {
            let new_state: TaskWakerState = new_state.into();
            assert_eq!(new_state, TaskWakerState::Notified);
            return Poll::Ready(());
        }

        unsafe {
            let will_wake = (&*self.waker.get())
                .as_ref()
                .map(|w| waker.will_wake(w))
                .unwrap_or(false);

            if !will_wake {
                *self.waker.get() = Some(waker.clone());
            }

            let state = match self.state.compare_exchange(
                TaskWakerState::Updating.into(),
                TaskWakerState::Ready.into(),
                Ordering::AcqRel,
                Ordering::Acquire,
            ) {
                Ok(_) => return Poll::Pending,
                Err(e) => TaskWakerState::from(e),
            };

            assert_eq!(state, TaskWakerState::Notified);
            *self.waker.get() = None;
            Poll::Ready(())
        }
    }

    fn detach(&self) {
        let state: TaskWakerState = self.state.load(Ordering::Acquire).into();
        match state {
            TaskWakerState::Empty | TaskWakerState::Notified => return,
            TaskWakerState::Updating => unreachable!("multiple poll threads on same TaskWaker"),
            TaskWakerState::Ready => {}
        }

        match self.state.compare_exchange(
            TaskWakerState::Ready.into(),
            TaskWakerState::Empty.into(),
            Ordering::Acquire,
            Ordering::Acquire,
        ) {
            Ok(_) => unsafe { *self.waker.get() = None },
            Err(new_state) => {
                let new_state: TaskWakerState = new_state.into();
                assert_eq!(new_state, TaskWakerState::Notified);
            }
        }
    }

    fn wake(&self) {
        let state: TaskWakerState = self
            .state
            .swap(TaskWakerState::Notified.into(), Ordering::AcqRel)
            .into();

        if state == TaskWakerState::Ready {
            mem::replace(unsafe { &mut *self.waker.get() }, None)
                .expect("TaskWakerState was ready without a Waker")
                .wake();
        }
    }
}

pub struct Task {
    pub next: AtomicPtr<Self>,
    pub vtable: &'static TaskVTable,
    pub _pinned: PhantomPinned,
}

pub struct TaskVTable {
    pub clone_fn: unsafe fn(NonNull<Task>),
    pub drop_fn: unsafe fn(NonNull<Task>),
    pub wake_fn: unsafe fn(NonNull<Task>, bool),
    pub poll_fn: unsafe fn(NonNull<Task>, &Arc<Pool>, usize),
    pub join_fn: unsafe fn(NonNull<Task>, Option<(&Waker, *mut ())>) -> Poll<()>,
}

enum TaskData<F: Future> {
    Polling(F),
    Ready(F::Output),
    Joined,
}

#[derive(Copy, Clone, Debug, Eq, PartialEq)]
enum TaskStatus {
    Idle = 0,
    Scheduled = 1,
    Running = 2,
    Notified = 3,
    Ready = 4,
}

impl From<usize> for TaskStatus {
    fn from(value: usize) -> Self {
        match value {
            0 => Self::Idle,
            1 => Self::Scheduled,
            2 => Self::Running,
            3 => Self::Notified,
            4 => Self::Ready,
            _ => unreachable!("invalid TaskStatus"),
        }
    }
}

#[derive(Copy, Clone, Debug)]
struct TaskState {
    pool: Option<NonNull<Pool>>,
    status: TaskStatus,
}

impl From<usize> for TaskState {
    fn from(value: usize) -> Self {
        Self {
            pool: NonNull::new((value & !0b111usize) as *mut Pool),
            status: TaskStatus::from(value & 0b111),
        }
    }
}

impl Into<usize> for TaskState {
    fn into(self) -> usize {
        let pool = self.pool.map(|p| p.as_ptr() as usize).unwrap_or(0);
        let status = self.status as usize;
        pool | status
    }
}

pub(crate) struct TaskFuture<F: Future> {
    task: Task,
    waker: TaskWaker,
    state: AtomicUsize,
    data: UnsafeCell<TaskData<F>>,
}

impl<F: Future> Drop for TaskFuture<F> {
    fn drop(&mut self) {
        let state: TaskState = self.state.load(Ordering::Relaxed).into();
        assert_eq!(state.status, TaskStatus::Ready);
        assert_eq!(state.pool, None);
    }
}

impl<F: Future> TaskFuture<F> {
    const TASK_VTABLE: TaskVTable = TaskVTable {
        clone_fn: Self::on_clone,
        drop_fn: Self::on_drop,
        wake_fn: Self::on_wake,
        poll_fn: Self::on_poll,
        join_fn: Self::on_join,
    };

    pub fn spawn(pool: &Arc<Pool>, worker_index: usize, future: F) -> JoinHandle<F::Output> {
        let this = Arc::pin(Self {
            task: Task {
                next: AtomicPtr::new(ptr::null_mut()),
                vtable: &Self::TASK_VTABLE,
                _pinned: PhantomPinned,
            },
            waker: TaskWaker::default(),
            state: AtomicUsize::new(
                TaskState {
                    pool: Some(NonNull::from(pool.as_ref())),
                    status: TaskStatus::Scheduled,
                }
                .into(),
            ),
            data: UnsafeCell::new(TaskData::Polling(future)),
        });

        unsafe {
            let this = Pin::into_inner_unchecked(this);
            let task = NonNull::from(&this.task);
            mem::forget(this.clone()); // keep a reference for JoinHandle
            mem::forget(this); // keep a reference for ourselves

            pool.mark_task_begin();
            pool.emit(PoolEvent::TaskSpawned { worker_index, task });
            pool.schedule(Some(worker_index), task, false);

            JoinHandle {
                task: Some(task),
                _phantom: PhantomData,
            }
        }
    }

    unsafe fn from_task(task: NonNull<Task>) -> NonNull<Self> {
        let task_offset = {
            let stub = MaybeUninit::<Self>::uninit();
            let base_ptr = stub.as_ptr();
            let field_ptr = ptr::addr_of!((*base_ptr).task);
            (field_ptr as usize) - (base_ptr as usize)
        };

        let self_offset = (task.as_ptr() as usize) - task_offset;
        let self_ptr = NonNull::new(self_offset as *mut Self);
        self_ptr.expect("invalid Task ptr for container_of")
    }

    unsafe fn with<T>(task: NonNull<Task>, f: impl FnOnce(Pin<&Self>) -> T) -> T {
        let self_ptr = Self::from_task(task);
        f(Pin::new_unchecked(self_ptr.as_ref()))
    }

    unsafe fn on_clone(task: NonNull<Task>) {
        let this = Arc::from_raw(task.as_ptr());
        mem::forget(this.clone());
        mem::forget(this)
    }

    unsafe fn on_drop(task: NonNull<Task>) {
        let this = Arc::from_raw(task.as_ptr());
        mem::drop(this)
    }

    unsafe fn on_wake(task: NonNull<Task>, also_drop: bool) {
        let pool_ptr = match Self::with(task, |this| {
            this.state
                .fetch_update(Ordering::AcqRel, Ordering::Relaxed, |state| {
                    let mut state: TaskState = state.into();
                    if state.status != TaskStatus::Ready {
                        assert_ne!(state.pool, None);
                    }

                    state.status = match state.status {
                        TaskStatus::Running => TaskStatus::Notified,
                        TaskStatus::Idle => TaskStatus::Scheduled,
                        _ => return None,
                    };
                    Some(state.into())
                })
                .map(TaskState::from)
        }) {
            Err(_) => None,
            Ok(state) => match state.status {
                TaskStatus::Running => None,
                TaskStatus::Idle => state.pool,
                status => unreachable!("invalid task status when waking {:?}", status),
            },
        };

        if let Some(pool_ptr) = pool_ptr {
            let be_fair = false;
            Pool::with_current(|pool, index| {
                // We're inside the pool, schedule from a worker thread
                pool.emit(PoolEvent::TaskScheduled {
                    worker_index: Some(index),
                    task,
                });
                pool.schedule(Some(index), task, be_fair)
            })
            .unwrap_or_else(|| {
                // We're outside the pool, schedule to a random worker thread.
                //
                // If we transitioned from TaskStatus::Idle to TaskStatus::Scheduled,
                // then the Pool must still be alive since the TaskFuture hasn't completed yet (Ready).
                //
                // This means it should be safe to deref the pool from here,
                // but we have to be careful not to drop the Arc<Pool> since it was never cloned.
                let pool = Arc::from_raw(pool_ptr.as_ptr());
                pool.emit(PoolEvent::TaskScheduled {
                    worker_index: None,
                    task,
                });
                pool.schedule(None, task, be_fair);
                mem::forget(pool);
            });
        }

        if also_drop {
            Self::on_drop(task);
        }
    }

    unsafe fn on_poll(task: NonNull<Task>, pool: &Arc<Pool>, worker_index: usize) {
        let pool_ptr = NonNull::from(pool.as_ref());

        let poll_result = Self::with(task, |this| {
            let mut state: TaskState = this.state.load(Ordering::Relaxed).into();
            match state.status {
                TaskStatus::Scheduled | TaskStatus::Notified => {}
                TaskStatus::Idle => unreachable!("polling task when idle"),
                TaskStatus::Running => unreachable!("polling task when already running"),
                TaskStatus::Ready => unreachable!("polling task when already completed"),
            }

            assert_eq!(state.pool, Some(pool_ptr));
            state.status = TaskStatus::Running;
            this.state.store(state.into(), Ordering::Relaxed);

            pool.emit(PoolEvent::TaskPolling { worker_index, task });
            let poll_result = this.poll_future();
            pool.emit(PoolEvent::TaskPolled { worker_index, task });

            poll_result
        });

        let poll_output = match poll_result {
            Poll::Ready(output) => output,
            Poll::Pending => {
                match Self::with(task, |this| {
                    let current_state = TaskState {
                        pool: Some(pool_ptr),
                        status: TaskStatus::Running,
                    };

                    let new_state = TaskState {
                        pool: Some(pool_ptr),
                        status: TaskStatus::Idle,
                    };

                    this.state
                        .compare_exchange(
                            current_state.into(),
                            new_state.into(),
                            Ordering::AcqRel,
                            Ordering::Relaxed,
                        )
                        .map_err(TaskState::from)
                }) {
                    Ok(_) => {
                        return pool.emit(PoolEvent::TaskIdling { worker_index, task });
                    }
                    Err(state) => {
                        let be_fair = true;
                        assert_eq!(state.pool, Some(pool_ptr));
                        assert_eq!(state.status, TaskStatus::Notified);
                        return pool.schedule(Some(worker_index), task, be_fair);
                    }
                }
            }
        };

        Self::with(task, |this| {
            match mem::replace(&mut *this.data.get(), TaskData::Ready(poll_output)) {
                TaskData::Polling(future) => mem::drop(future),
                TaskData::Ready(_) => unreachable!("TaskData already had an output value"),
                TaskData::Joined => unreachable!("TaskData already joind when setting output"),
            }

            let new_state = TaskState {
                pool: None,
                status: TaskStatus::Ready,
            };

            this.state.store(new_state.into(), Ordering::Release);
            this.waker.wake();
        });

        pool.mark_task_end();
        pool.emit(PoolEvent::TaskShutdown { worker_index, task });
        Self::on_drop(task)
    }

    unsafe fn poll_future(&self) -> Poll<F::Output> {
        unsafe fn waker_vtable(ptr: *const (), f: impl FnOnce(&'static TaskVTable, NonNull<Task>)) {
            let task_ptr = ptr as *const Task as *mut Task;
            let task = NonNull::new(task_ptr).expect("invalid task ptr for Waker");
            let vtable = task.as_ref().vtable;
            f(vtable, task)
        }

        const WAKER_VTABLE: RawWakerVTable = RawWakerVTable::new(
            |ptr| unsafe {
                waker_vtable(ptr, |vtable, task| (vtable.clone_fn)(task));
                RawWaker::new(ptr, &WAKER_VTABLE)
            },
            |ptr| unsafe { waker_vtable(ptr, |vtable, task| (vtable.wake_fn)(task, true)) },
            |ptr| unsafe { waker_vtable(ptr, |vtable, task| (vtable.wake_fn)(task, false)) },
            |ptr| unsafe { waker_vtable(ptr, |vtable, task| (vtable.drop_fn)(task)) },
        );

        let ptr = NonNull::from(&self.task).as_ptr() as *const ();
        let raw_waker = RawWaker::new(ptr, &WAKER_VTABLE);
        let waker = Waker::from_raw(raw_waker);

        let poll_output = match &mut *self.data.get() {
            TaskData::Polling(ref mut future) => {
                let mut context = Context::from_waker(&waker);
                Pin::new_unchecked(future).poll(&mut context)
            }
            TaskData::Ready(_) => unreachable!("TaskData polled when already ready"),
            TaskData::Joined => unreachable!("TaskData polled when already joined"),
        };

        mem::forget(waker); // don't drop since we didn't clone it
        poll_output
    }

    unsafe fn on_join(task: NonNull<Task>, context: Option<(&Waker, *mut ())>) -> Poll<()> {
        let (waker, output_ptr) = match context {
            Some(context) => context,
            None => {
                Self::with(task, |this| this.waker.detach());
                Self::on_drop(task);
                return Poll::Ready(());
            }
        };

        match Self::with(task, |this| this.waker.poll(waker)) {
            Poll::Pending => return Poll::Pending,
            Poll::Ready(_) => {}
        }

        Self::with(task, |this| {
            let state: TaskState = this.state.load(Ordering::Acquire).into();
            assert_eq!(state.status, TaskStatus::Ready);
            assert_eq!(state.pool, None);

            match mem::replace(&mut *this.data.get(), TaskData::Joined) {
                TaskData::Polling(_) => unreachable!("TaskData joined while still polling"),
                TaskData::Ready(output) => ptr::write(output_ptr as *mut _, output),
                TaskData::Joined => unreachable!("TaskData already joined when joining"),
            }
        });

        Self::on_drop(task);
        Poll::Ready(())
    }
}

pub fn spawn<F>(future: F) -> JoinHandle<F::Output>
where
    F: Future + Send + 'static,
    F::Output: Send + 'static,
{
    Pool::with_current(|pool, worker_index| TaskFuture::spawn(pool, worker_index, future))
        .unwrap_or_else(|| unreachable!("spawn() called outside of thread pool"))
}

pub struct JoinHandle<T> {
    task: Option<NonNull<Task>>,
    _phantom: PhantomData<T>,
}

unsafe impl<T: Send> Send for JoinHandle<T> {}

impl<T> Drop for JoinHandle<T> {
    fn drop(&mut self) {
        if let Some(task) = self.task {
            unsafe {
                let vtable = task.as_ref().vtable;
                let _ = (vtable.join_fn)(task, None);
            }
        }
    }
}

impl<T> Future for JoinHandle<T> {
    type Output = T;

    fn poll(self: Pin<&mut Self>, ctx: &mut Context<'_>) -> Poll<Self::Output> {
        unsafe {
            let mut_self = Pin::get_unchecked_mut(self);
            let task = mut_self.task.expect("JoinHandle polled after completion");

            let mut output = MaybeUninit::<T>::uninit();
            let join_context = (ctx.waker(), output.as_mut_ptr() as *mut ());

            let vtable = task.as_ref().vtable;
            if let Poll::Pending = (vtable.join_fn)(task, Some(join_context)) {
                return Poll::Pending;
            }

            mut_self.task = None;
            Poll::Ready(output.assume_init())
        }
    }
}

pub fn yield_now() -> impl Future<Output = ()> {
    struct YieldNow {
        yielded: bool,
    }

    impl Future for YieldNow {
        type Output = ();

        fn poll(mut self: Pin<&mut Self>, ctx: &mut Context<'_>) -> Poll<()> {
            if mem::replace(&mut self.yielded, true) {
                return Poll::Ready(());
            }

            ctx.waker().wake_by_ref();
            Poll::Pending
        }
    }

    YieldNow { yielded: false }
}
