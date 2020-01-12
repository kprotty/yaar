use super::task::*;
use core::{cell::UnsafeCell, marker::Sync, mem};

#[cfg(feature = "serial")]
pub mod serial;

/// Abstraction over providing execution of runtime [`Task`]s.
pub trait Executor: Sync {
    // ~ brainstorming
    // fn blocking(&self, task: &mut Task, cfg(time) Option<Duration>) -> bool;

    /// Mark the given task as runnable and execute it eventually in the future.
    fn schedule(&self, task: &mut Task);
}

/// A virtual reference to the current global scoped Executor.
#[derive(Debug, PartialEq)]
pub struct ExecutorRef {
    ptr: *const (),
    _schedule: unsafe fn(*const (), task: *mut Task),
}

impl ExecutorRef {
    /// Proxy for [`Executor::schedule`].
    #[inline]
    pub fn schedule(&self, task: &mut Task) {
        unsafe { (self._schedule)(self.ptr, task) }
    }
}

struct ExecutorCell(UnsafeCell<Option<&'static ExecutorRef>>);

unsafe impl Sync for ExecutorCell {}

static EXECUTOR_CELL: ExecutorCell = ExecutorCell(UnsafeCell::new(None));

/// Set the global executor instance for a given function scope.
/// The global executor supports being modified recursively as it
/// is restored via stack but should not be called in parellel.
pub fn with_executor_as<E: Executor, T>(executor: &E, scoped: impl FnOnce(&E) -> T) -> T {
    // The vtable for the provided executor.
    // Wish it could be `const` but rust doesnt currently
    // support function type parameters in const :(
    let executor_ref = ExecutorRef {
        ptr: executor as *const _ as *const (),
        _schedule: |ptr, task| unsafe { (&*(ptr as *const E)).schedule(&mut *task) },
    };

    // promote the executor_ref to &'static ExecutorRef for storage in the global executor cell.
    let static_ref = unsafe { Some(&*(&executor_ref as *const _)) };
    let old_ref = unsafe { mem::replace(&mut *EXECUTOR_CELL.0.get(), static_ref) };
    let result = scoped(executor);
    let our_ref = unsafe { mem::replace(&mut *EXECUTOR_CELL.0.get(), old_ref) };

    // ensure that the stack of executor_ref's invariant is maintained.
    debug_assert_eq!(our_ref, static_ref);
    result
}

pub fn with_executor<T>(scoped: impl FnOnce(&ExecutorRef) -> T) -> T {
    scoped(unsafe {
        (&*EXECUTOR_CELL.0.get())
            .as_ref()
            .expect("Executor is not set. Make sure this is invoked only inside the scope of `with_executor_as()`")
    })
}
