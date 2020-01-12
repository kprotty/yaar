use super::{super::task, with_executor_as, Executor};
use core::{cell::UnsafeCell, future::Future, marker::Sync};

/// Run a future, and all the tasks is spawns recursively,
/// using an executor optimized for running single threaded.
///
/// As this relies on `with_executor_as()`, the caller should
/// ensure that this function is not called on multiple threads.
pub fn run<T>(future: impl Future<Output = T>) -> T {
    let executor = SerialExecutor(UnsafeCell::new(Runtime {
        run_queue: task::List::default(),
    }));

    with_executor_as(&executor, move |executor| {
        let mut future = task::FutureTask::new(task::Priority::Normal, future);
        if future.resume().is_ready() {
            return future.into_output().unwrap();
        }

        while let Some(task) = executor.poll() {
            task.resume();
        }

        debug_assert!(future.resume().is_ready());
        future.into_output().unwrap()
    })
}

struct Runtime {
    run_queue: task::List,
}

/// Encapsulate the runtime so that it can be accessed from a global state.
/// Should be safe to do so without synchronization considering it should
/// only be accessed by a single thread.  
struct SerialExecutor(UnsafeCell<Runtime>);

unsafe impl Sync for SerialExecutor {}

impl SerialExecutor {
    pub fn poll<'a>(&self) -> Option<&'a mut task::Task> {
        let runtime = unsafe { &mut *self.0.get() };
        runtime.run_queue.pop()
    }
}

impl Executor for SerialExecutor {
    fn schedule(&self, task: &mut task::Task) {
        let mut list = task::PriorityList::default();
        list.push(task);

        let runtime = unsafe { &mut *self.0.get() };
        runtime.run_queue.push(&list);
    }
}
