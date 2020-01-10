use super::task::{TaskList};
use core::{
    future::Future,
};

#[cfg(feature = "rt-alloc")]
use core::alloc::GlobalAlloc;

pub fn run<T>(
    #[cfg(feature = "rt-alloc")] allocator: impl GlobalAlloc,
    future: impl Future<Output = T>,
) -> T {
    #[cfg(not(feature = "rt-alloc"))] let allocator = ();

    let mut executor = Executor {
        allocator,
        run_queue: TaskList::default(),
    };
}

struct Executor<A> {
    allocator: A,
    run_queue: TaskList,
}
