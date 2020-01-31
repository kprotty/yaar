use super::task::Task;
use core::{cell::Cell, fmt::Debug};

pub trait Executor: Sync + Debug {
    /// Schedule the task to be eventually executed.
    fn submit(&self, task: &mut Task);
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
    assert_eq!(
        Some(our_ref as *const _),
        EXECUTOR_REF
            .0
            .replace(old_ref)
            .map(|ptr| ptr as *const _ as *const E),
        "The executor stack was somehow malformed",
    );
    result
}

/// Run the provided function with a referece to the global executor.
///
/// # Safety
///
/// Internal usage should not persist the global executor reference as
/// the static lifetime serves only as a placebo to indicate that the reference
/// is global meaning there are no guarantees on its validity after the function
/// ends.
pub(crate) fn with_executor<T>(f: impl FnOnce(&'static dyn Executor) -> T) -> T {
    f(EXECUTOR_REF
        .0
        .get()
        .expect("Tried to get the executor without being in the scope of `with_executor_as`"))
}
