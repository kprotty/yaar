use super::task::*;
use core::{
    mem,
    marker::Sync,
    cell::UnsafeCell,
};

#[cfg(feature = "rt-serial")]
pub mod serial;


pub trait Executor: Sync {
    fn schedule(&self, task: &mut Task);
}

pub struct ExecutorRef {
    ptr: *const (),
    vtable: &'static ExecutorVTable,
}

struct ExecutorVTable {
    schedule: unsafe fn(*const (), task: *mut Task),
}

impl ExecutorRef {
    #[inline]
    pub fn schedule(&self, task: &mut Task) {
        unsafe { (self.vtable.schedule)(self.ptr, task) }
    }
}

struct ExecutorCell(UnsafeCell<Option<ExecutorRef>>);

unsafe impl Sync for ExecutorCell {}

/// Global reference to the current executor implementation.
/// Only modified by with_executor_as() which should not be called
/// via multiple threads to its safe to use a mutable global.
static EXECUTOR_CELL: ExecutorCell = ExecutorCell(UnsafeCell::new(None));

pub fn with_executor_as<E: Executor, T>(executor: &E, scoped: impl FnOnce(&E) -> T) -> T {
    // would have been const but "can't use generic parameters from outer function"
    let vtable = ExecutorVTable {
        schedule: |ptr, task| unsafe {
            (&*(ptr as *const E)).schedule(&mut *task)
        },
    };

    unsafe {
        // promote our local vtable to static so it can be accessed from a global setting
        let old_ref = mem::replace(
            &mut *EXECUTOR_CELL.0.get(),
            Some(ExecutorRef {
                ptr: executor as *const E as *const (),
                vtable: &*(&vtable as *const _),
            }),
        );

        let result = scoped(executor);

        // restore the old executor and make sure ours was on-top of the stack
        let our_ref = mem::replace(&mut *EXECUTOR_CELL.0.get(), old_ref).unwrap();
        debug_assert_eq!(our_ref.ptr as usize, executor as *const _ as usize);
        debug_assert_eq!(our_ref.vtable as *const _ as usize, &vtable as *const _ as usize);

        result
    }
}

pub fn with_executor<T>(scoped: impl FnOnce(&ExecutorRef) -> T) -> T {
    scoped(unsafe {
        (&*EXECUTOR_CELL.0.get())
            .as_ref()
            .expect("Executor is not set. Make sure this is invoked only inside the scope of `with_executor_as()`")
    })
}
