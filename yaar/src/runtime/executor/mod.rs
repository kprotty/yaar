pub mod scheduler;

use super::task::Task;
use core::{any::Any, cell::Cell, ptr::NonNull};

pub trait Executor: Any + Sync {
    /// Schedule the task to be eventually executed.
    fn schedule(&self, task: &Task);
}

/// Reference to the current running executor.
static EXECUTOR_REF: ExecutorRef = ExecutorRef(Cell::new(None));
struct ExecutorRef(Cell<Option<NonNull<dyn Executor>>>);
unsafe impl Sync for ExecutorRef {}

/// For the duration of the function provided, use the given executor as the
/// global executor which serves as the interface for runtime internals to the
/// scheduler.
///
/// # Safety
///
/// This is not thread safe as it assumes owned access to the global executor
/// reference.
pub fn with_executor_as<E: Executor, T>(executor: &E, f: impl FnOnce(&E) -> T) -> T {
    let our_ref = (executor as &dyn Executor) as *const _ as *mut _;
    let old_ref = EXECUTOR_REF.0.replace(NonNull::new(our_ref));
    let result = f(executor);
    EXECUTOR_REF.0.set(old_ref);
    result
}

/// Run the provided function with a referece to the global executor.
/// Returns `None` if not called in the scope of [`with_executor_as`].
///
/// # Safety
///
/// Usage should not persist the global executor reference as there are
/// no guarantees on its validity after the function ends.
pub fn with_executor<T>(f: impl FnOnce(&dyn Executor) -> T) -> Option<T> {
    EXECUTOR_REF.0.get().map(|ptr| f(unsafe { &*ptr.as_ptr() }))
}
