pub mod node;

use super::task::Task;
use core::{cell::Cell, fmt::Debug};

pub trait Executor: Sync + Debug {
    /// Schedule the task to be eventually executed.
    fn schedule(&self, task: &Task);
}

/// Reference to the current running executor.
static EXECUTOR_REF: ExecutorRef = ExecutorRef(Cell::new(None));

#[derive(Debug)]
struct ExecutorRef(Cell<Option<&'static dyn Executor>>);

unsafe impl Sync for ExecutorRef {}

/// For the duration of the function provided, use the given executor as the
/// global executor which serves as the interface for runtime internals to the
/// scheduler.
///
/// # Safety
///
/// This is not thread safe as it assumes owned access to the global executor
/// reference.
pub fn with_executor_as<E: Executor + 'static, T>(executor: &E, f: impl FnOnce(&E) -> T) -> T {
    let our_ref = unsafe { &*(executor as *const E) };
    let old_ref = EXECUTOR_REF.0.replace(Some(our_ref));
    let result = f(executor);
    EXECUTOR_REF.0.set(old_ref);
    result
}

/// Run the provided function with a referece to the global executor.
/// Returns `None` if not called in the scope of [`with_executor_as`].
///
/// # Safety
///
/// Internal usage should not persist the global executor reference as
/// the static lifetime serves only as a placebo to indicate that the reference
/// is global meaning there are no guarantees on its validity after the function
/// ends.
pub(crate) fn with_executor<T>(f: impl FnOnce(&'static dyn Executor) -> T) -> Option<T> {
    EXECUTOR_REF.0.get().map(f)
}
