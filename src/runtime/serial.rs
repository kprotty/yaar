use super::task;
use core::{
    future::Future,
    cell::UnsafeCell,
};

#[cfg(feature = "rt-alloc")]
use core::alloc::GlobalAlloc;

pub fn run<T>(
    #[cfg(feature = "rt-alloc")] allocator: impl GlobalAlloc,
    future: impl Future<Output = T>,
) -> T {
    #[cfg(not(feature = "rt-alloc"))] let allocator = ();

    let executor = Executor(UnsafeCell::new(Runtime {
        allocator,
        run_queue: task::List::default(),
    }));

    task::with_executor_as(&executor, move |executor| {
        let mut future = task::TaskFuture::new(task::Priority::Normal, future);
        let _ = future.poll();

        let runtime = unsafe { &mut *executor.0.get() };
        while let Some(task) = runtime.run_queue.pop() {
            task.resume();
        }

        future.into_inner().unwrap()
    })
}

struct Executor<A>(UnsafeCell<Runtime<A>>);

unsafe impl<A> Sync for Executor<A> {}

impl<A> task::Executor for Executor<A> {
    fn schedule(&self, task: &mut task::Task) {
        let mut list = task::PriorityList::default();
        list.push(task);

        let runtime = unsafe { &mut *self.0.get() };
        runtime.run_queue.push(&list);
    }
}

struct Runtime<A> {
    #[cfg_attr(not(feature = "rt-alloc"), allow(dead_code))] allocator: A,
    run_queue: task::List,
}
