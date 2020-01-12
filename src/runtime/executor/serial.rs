use super::{super::task, with_executor_as, Executor};
use core::{cell::UnsafeCell, future::Future, marker::Sync};

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
