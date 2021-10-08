use crate::{
    internals::{container_of::container_of, waker::AtomicWaker},
    runtime::{pool::Pool, queue::Node, worker::WorkerRef},
    task::JoinHandle,
};
use std::{
    any::Any,
    cell::UnsafeCell,
    future::Future,
    pin::Pin,
    ptr::NonNull,
    sync::atomic::{AtomicUsize, Ordering},
    task::{Context, Poll, RawWaker, RawWakerVTable, Waker},
};

pub(super) struct Task {
    pub(super) node: Node,
    pub(super) vtable: &'static TaskVTable,
}

impl Task {
    /// Upgrades a reference to a Node into a reference to a Task.
    ///
    /// # Safety
    ///
    /// This assumes the Node reference is valid field of a properly pinned Task
    pub(super) unsafe fn from_node<'a>(node: Pin<&'a Node>) -> Pin<&'a Self> {
        let self_ptr = container_of!(Self, node, (&*node as *const Node));
        Pin::new_unchecked(&*self_ptr.as_ptr())
    }
}

struct TaskVTable {
    clone_fn: fn(task: NonNull<Task>),
    drop_fn: fn(task: Pin<&Task>),
    wake_fn: fn(task: Pin<&Task>, wake_by_ref: bool),
    pub(super) poll_fn: fn(task: Pin<&Task>, worker_ref: &WorkerRef),
    pub(crate) join_fn: fn(task: Pin<&Task>, waker: &Waker, output: NonNull<()>) -> Poll<()>,
    pub(crate) detach_fn: fn(task: Pin<&Task>),
    pub(crate) consume_fn: fn(task: Pin<&Task>, output: NonNull<()>),
}

#[derive(Copy, Clone, Debug, Eq, PartialEq)]
enum TaskStatus {
    Idle = 0,
    Scheduled = 1,
    Running = 2,
    Notified = 3,
}

impl Into<usize> for TaskStatus {
    fn into(self) -> usize {
        self as usize
    }
}

impl From<usize> for TaskStatus {
    fn from(value: usize) -> Self {
        [Self::Idle, Self::Scheduled, Self::Running, Self::Notified][value]
    }
}

#[derive(Copy, Clone, Debug, Eq, PartialEq)]
struct TaskState {
    pool: Option<NonNull<Pool>>,
    status: TaskStatus,
}

impl TaskState {
    const COMPLETED: Self = Self {
        pool: None,
        status: TaskStatus::Notified,
    };
}

impl Into<usize> for TaskState {
    fn into(self) -> Self {
        let pool_ptr = self.pool.map(|p| p.as_ptr() as usize).unwrap_or(0);
        assert_eq!(pool_ptr & 0b11, 0);
        pool_ptr | self.status.into()
    }
}

impl From<usize> for TaskState {
    fn from(value: usize) -> Self {
        Self {
            pool: NonNull::new((value & !0b11) as *mut Pool),
            status: TaskStatus::from(value & 0b11),
        }
    }
}

enum TaskData<F: Future> {
    Polling(F),
    Ready(Result<F::Output, Box<dyn Any + Send>>),
    Consumed,
}

pub(crate) struct TaskFuture<F: Future> {
    task: Task,
    waker: AtomicWaker,
    state: AtomicUsize,
    data: UnsafeCell<TaskData<F>>,
}

impl<F: Future> TaskState<F> {
    const TASK_VTABLE: TaskVTable = TaskVTable {
        clone_fn: Self::on_clone,
        drop_fn: Self::on_drop,
        wake_fn: Self::on_wake,
        poll_fn: Self::on_poll,
        join_fn: Self::on_join,
        detach_fn: Self::on_detach,
        consume_fn: Self::on_consume,
    };

    pub(crate) fn spawn(worker_ref: &WorkerRef, future: F) -> JoinHandle<F::Output> {
        // SAFETY:
        // - Pin::into_inner_unchecked(TaskFuture):
        //   we promise to treat memory as pinned and not move it until the last reference is dropped.
        //
        // - Pin::new_unchecked(Task):
        //   we promise to treat the memory as pinned until the TaskFuture is dropped.
        //
        // - JoinHandle::new(Task):
        //   we promise to keep the task reference alive until the JoinHandle is either consumed, dropped or polled to completion.
        //
        // - WorkerRef::schedule(Task):
        //   same guarantee from the Pin::new_unchecked above
        unsafe {
            let pinned = Pin::into_inner_unchecked(Arc::pin(Self {
                task: Task {
                    node: Node::default(),
                    vtable: &Self::TASK_VTABLE,
                },
                waker: AtomicWaker::default(),
                state: AtomicUsize::new(0),
                data: UnsafeCell::new(TaskData::Polling(future)),
            }));

            let task = Pin::new_unchecked(&pinned.task);
            let join_handle = JoinHandle::new(task.as_ref());
            mem::forget(Arc::clone(&pinned));

            mem::forget(pinned);
            worker_ref.mark_task_begin();
            worker_ref.schedule(WorkerTask::Spawned(task));

            join_handle
        }
    }

    /// Upgrades a reference to a Task into a reference to this TaskFuture.
    ///
    /// # Safety
    ///
    /// This assumes the Task reference is valid field of the TaskFuture
    unsafe fn from_task<'a>(task: Pin<&'a Task>) -> Pin<&'a Self> {
        let self_ptr = container_of!(Self, task, (&*task as *const Task));
        Pin::new_unchecked(&*self_ptr.as_ptr())
    }

    fn on_clone(task: Pin<&Task>) {
        // SAFETY:
        // Only called by the TaskVTable for this TaskFuture, making from_task() sound.
        // Also, TaskFutures are only created via Arc::pin() so getting their Arc is sound.
        unsafe {
            let self_ptr = NonNull::from(&*Self::from_task(task));
            let self_arc = Arc::from_raw(self_ptr.as_ptr());
            mem::forget(Arc::clone(&self_arc));
            mem::forget(self_arc);
        }
    }

    fn on_drop(task: Pin<&Task>) {
        // SAFETY:
        // Only called by the TaskVTable & related functions for this TaskFuture, making from_task() sound.
        // Also, TaskFutures are only created via Arc::pin() so getting their Arc is sound.
        unsafe {
            let self_ptr = NonNull::from(&*Self::from_task(task)); // consumes the &Task, no dangling refs
            let self_arc = Arc::from_raw(self_ptr.as_ptr());
            mem::drop(self_arc) // decrement the TaskFuture ref count
        }
    }

    fn on_wake(task: Pin<&Task>, wake_by_ref: bool) {
        // SAFETY:
        // - Only called by the TaskVTable for this TaskFuture, making from_task() sound.
        // - on_drop() can be safely called for !wake_by_ref given that
        //   a schedule(task) occurs only if the Future is still active
        //   (ref_count > 2 given the Waker calling).
        unsafe {
            let result = Self::from_task(task.as_ref()).state.fetch_update(
                Ordering::AcqRel,
                Ordering::Relaxed,
                |state| {
                    let mut state = TaskState::from(state);
                    if state == TaskState::COMPLETED {
                        return None;
                    }

                    state.status = match state.status {
                        TaskStatus::Idle => TaskStatus::Scheduled,
                        TaskStatus::Scheduled => return None,
                        TaskStatus::Running => TaskStatus::Notified,
                        TaskStatus::Notified => return None,
                    };

                    Some(state.into())
                },
            );

            if let Ok(state) = result.map(TaskState::from) {
                if state.status == TaskStatus::Idle {
                    let pool = state.pool.expect("Task scheduled without a Pool");

                    WorkerRef::with_current(|worker_ref| {
                        worker_ref.schedule(WorkerTask::Scheduled(task));
                    })
                    .or_else(|| {
                        let pool = Arc::from_raw(pool.as_ptr() as *const Pool);
                        pool.schedule(WorkerTask::Injected(task));
                        mem::forget(pool);
                    });
                }
            }

            if !wake_by_ref {
                Self::on_drop(task);
            }
        }
    }

    fn on_poll(task: Pin<&Task>, worker_ref: &WorkerRef) {
        // SAFETY:
        // - schedule(Task) required unsafe to ensure that:
        //   - the Task stays alive until its scheduled & polled here
        //   - the Task is polled only by one thread at a time (&mut *data.get())
        unsafe {
            let this = Self::from_task(task.as_ref());

            let state: TaskState = this.state.load(Ordering::Relaxed).into();
            assert_eq!(state.pool, Some(NonNull::from(&*worker_ref.pool)));
            assert_eq!(state.status, TaskStatus::Scheduled);

            let poll_result = match &mut *this.data.get() {
                TaskData::Consumed => unreachable!("Future polling when already consumed"),
                TaskData::Ready(_) => unreachable!("Future polling when already polled"),
                TaskData::Polling(ref mut future) => {
                    unsafe fn waker_task<T>(ptr: *const (), f: impl FnOnce(Pin<&Task>) -> T) -> T {
                        f(Pin::new_unchecked(&*(ptr as *const Task)))
                    }

                    const WAKER_VTABLE: RawWakerVTable = RawWakerVTable::new(
                        |ptr| unsafe {
                            waker_task(ptr, |task| {
                                (task.vtable.clone_fn)(task);
                                RawWaker::new(ptr, &WAKER_VTABLE)
                            })
                        },
                        |ptr| unsafe {
                            waker_task(ptr, |task| (task.vtable.wake_fn)(task, false));
                        },
                        |ptr| unsafe {
                            waker_task(ptr, |task| (task.vtable.wake_fn)(task, true));
                        },
                        |ptr| unsafe {
                            waker_task(ptr, |task| (task.vtable.drop_fn)(task));
                        },
                    );

                    let ptr = &*task as *const Task as *const ();
                    let raw_waker = RawWaker::new(ptr, &WAKER_VTABLE);
                    let waker = Waker::from_raw(raw_waker);
                    let waker = ManuallyDrop::new(waker);

                    match std::panic::catch_unwind(|| {
                        let mut ctx = Context::from_waker(&*waker);
                        Pin::new_unchecked(future).poll(&mut ctx)
                    }) {
                        Ok(Poll::Pending) => Poll::Pending,
                        Ok(Poll::Ready(output)) => Poll::Ready(Ok(output)),
                        Err(panic_payload) => Poll::Ready(Err(panic_payload)),
                    }
                }
            };

            let result = match poll_result {
                Poll::Ready(result) => result,
                Poll::Pending => {
                    let become_idle = Self::from_task(task)
                        .state
                        .fetch_update(Ordering::Release, Ordering::Relaxed, |state| {
                            let mut state: TaskState = state.into();
                            assert_ne!(state, TaskState::COMPLETED);
                            assert_eq!(state.pool, Some(NonNull::from(&*waker_ref.pool)));

                            if state.status == TaskStatus::Notified {
                                state.status = TaskStatus::Scheduled;
                                return Some(state.into());
                            }

                            assert_eq!(state.status, TaskStatus::Running);
                            state.status = TaskStatus::Idle;
                            Some(state.into())
                        })
                        .map(TaskState::from)
                        .unwrap();

                    if become_idle.status == TaskStatus::Notified {
                        mem::drop(this);
                        worker_ref.schedule(WorkerTask::Yielded(task));
                    }

                    return;
                }
            };

            match mem::replace(&mut *this.data.get(), TaskData::Ready(result)) {
                TaskData::Consumed => unreachable!("Future completing when already consumed"),
                TaskData::Ready(_) => unreachable!("Future completing when already completed"),
                TaskData::Polling(future) => mem::drop(future),
            }

            // Release barrier ensures TaskData write is visible to on_consume
            let new_state: usize = TaskState::COMPLETED.into();
            this.state.store(new_state, Ordering::Release);
            this.waker.wake();

            worker_ref.pool.mark_task_complete();
            mem::drop(this); // on_drop() may free this so don't keep dangling reference
            Self::on_drop(task)
        }
    }

    fn on_join(task: Pin<&Task>, waker: &Waker, output: NonNull<()>) -> Poll<()> {
        // SAFETY:
        // Only called by JoinHandle::poll() which verifies that
        // - from_task() is valid here from JoinHandle::new()
        // - The caller ensures that they're the sole thread calling on_consume/on_join/on_detach
        unsafe {
            match Self::from_task(task).waker.poll(waker) {
                Poll::Pending => Poll::Pending,
                Poll::Ready(_) => Poll::Ready(Self::on_consume(output)),
            }
        }
    }

    fn on_detach(task: Pin<&Task>) {
        // SAFETY:
        // Only called by JoinHandle::drop() which verifies that
        // - from_task() is valid here from JoinHandle::new()
        // - The caller ensures that they're the sole thread calling on_consume/on_join/on_detach
        unsafe {
            Self::from_task(task.as_ref()).waker.detach();
            Self::on_drop(task)
        }
    }

    fn on_consume(task: Pin<&Task>, output: NonNull<()>) {
        // SAFETY:
        // Only called by JoinHandle::poll() or JoinHandle::consume() which verifies that
        // - from_task() is valid here from JoinHandle::new()
        // - The caller ensures that output is a NonNull<F::Output> which points to a valid F::Output.
        // - The caller ensures that they're the sole thread calling on_consume/on_join/on_detach
        unsafe {
            let this = Self::from_task(task.as_ref());
            let output_ptr = output.cast::<F::Output>().as_ptr();

            let state: TaskState = this.state.load(Ordering::Acquire).into();
            assert_eq!(
                state,
                TaskState::COMPLETED,
                "JoinHandle::consume() when future is not completed"
            );

            match mem::replace(&mut *this.data.get(), TaskData::Consumed) {
                TaskData::Consumed => unreachable!("Future already consumed when consuming"),
                TaskData::Polling(_) => unreachable!("Future being consumed while polling"),
                TaskData::Ready(Err(panic_payload)) => std::panic::resume_unwind(panic_payload),
                TaskData::Ready(Ok(output)) => ptr::write(output_ptr, output),
            }

            mem::drop(this); // on_drop() may free this, so don't keep dangling refs around
            Self::on_drop(task)
        }
    }
}
