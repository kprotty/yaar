use super::task::*;
use core::{cell::UnsafeCell, marker::Sync, mem};

#[cfg(feature = "rt-serial")]
pub mod serial;

pub trait Executor: Sync {
    fn schedule(&self, task: &mut Task);
}

pub struct ExecutorRef {
    ptr: *const (),
    _schedule: unsafe fn(*const (), task: &mut Task),
}

impl ExecutorRef {
    #[inline]
    pub fn schedule(&self, task: &mut Task) {
        unsafe { (self._schedule)(self.ptr, task) }
    }
}

struct ExecutorCell(UnsafeCell<Option<ExecutorRef>>);

unsafe impl Sync for ExecutorCell {}

static EXECUTOR_CELL: ExecutorCell = ExecutorCell(UnsafeCell::new(None));

pub fn with_executor_as<E: Executor, T>(executor: &E, scoped: impl FnOnce(&E) -> T) -> T {
    let old_ref = mem::replace(
        unsafe { &mut *EXECUTOR_CELL.0.get() },
        Some(ExecutorRef {
            ptr: executor as *const E as *const (),
            _schedule: |ptr, task| unsafe { (&*(ptr as *const E)).schedule(task) },
        }),
    );

    let result = scoped(executor);
    unsafe { *(&mut *EXECUTOR_CELL.0.get()) = old_ref };
    result
}

pub fn with_executor<T>(scoped: impl FnOnce(&ExecutorRef) -> T) -> T {
    scoped(unsafe {
        (&*EXECUTOR_CELL.0.get())
            .as_ref()
            .expect("Executor is not set. Make sure this is invoked only inside the scope of `with_executor_as()`")
    })
}
