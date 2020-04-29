use core::{
    fmt,
    mem::MaybeUninit,
    cell::Cell,
    alloc::Layout,
};

#[derive(Copy, Clone, Eq, PartialEq)]
pub(crate) enum TaskEvent {
    Clone,
    Wake,
    WakeByRef,
    Drop,
    Poll,
}

pub unsafe trait TaskEventHandler {
    fn on_waker_clone(&self);

    fn on_waker_wake(&self);
    
    fn on_waker_wake_by_ref(&self);

    fn on_waker_drop(&self);

    fn on_task_poll(&self);
}

#[derive(Debug)]
pub struct UncheckedTaskHandler(());

impl UncheckedTaskHandler {
    pub unsafe fn new() -> Self {
        Self(())
    }
}

unsafe impl TaskEventHandler for UncheckedTaskHandler {
    fn on_waker_clone(&self) {}
    fn on_waker_wake(&self) {}
    fn on_waker_wake_by_ref(&self) {}
    fn on_waker_drop(&self) {}
    fn on_task_poll(&self) {}
}

#[derive(Debug)]
pub struct RefCellTaskHandler {
    ref_count: Cell<usize>,
}

impl RefCellTaskHandler {
    pub unsafe fn new() -> Self {
        Self {
            ref_count: Cell::new(1),
        }
    }

    fn add_reference(&self) {
        self.ref_count.set(self
            .ref_count
            .get()
            .checked_add(1)
            .expect("refcount overflow")
        );
    }

    fn remove_reference(&self) {
        self.ref_count.set(self
            .ref_count
            .get()
            .checked_sub(1)
            .expect("refcount underflow")
        );
    }
}

impl Drop for RefCellTaskHandler {
    fn drop(&mut self) {
        self.remove_reference();
        if self.ref_count.get() != 0 {
            unreachable!("Task was dropped with existing Waker references");
        }
    }
}

unsafe impl TaskEventHandler for RefCellTaskHandler {
    fn on_task_poll(&self) {}
    fn on_waker_wake(&self) {}
    fn on_waker_wake_by_ref(&self) {}

    fn on_waker_clone(&self) {
        self.add_reference();
    }

    fn on_waker_drop(&self) {
        self.remove_reference();
    }
}

pub struct RcTaskHandler<DropFn> {
    drop_fn: Cell<MaybeUninit<DropFn>>,
    ref_count: Cell<usize>,
}

impl<DropFn> fmt::Debug for RcTaskHandler<DropFn> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("RcTaskHandler")
            .field("ref_count", &self.ref_count.get())
            .finish()
    }
}

impl<DropFn: FnOnce(*mut u8, Layout)> RcTaskHandler<DropFn> {
    pub unsafe fn new(drop_fn: DropFn) -> Self {
        Self {
            drop_fn: Cell::new(MaybeUninit::new(drop_fn)),
            ref_count: Cell::new(1),
        }
    }

    fn add_reference(&self) {
        self.ref_count.set(self
            .ref_count
            .get()
            .checked_add(1)
            .expect("refcount overflow")
        );
    }

    fn remove_reference(&self) {
        let ref_count = self
            .ref_count
            .get()
            .checked_sub(1)
            .expect("refcount underflow");
        
        if ref_count == 0 {
            let drop_fn = self.drop_fn.replace(MaybeUninit::uninit());
            let drop_fn = unsafe { drop_fn.assume_init() };
            drop_fn(self as *const _ as *mut _, Layout::<Self>::new());
        } else {
            self.ref_count.set(ref_count);
        }
    }
}

impl<DropFn: FnOnce(*mut u8, Layout)> Drop for RcTaskHandler<DropFn> {
    fn drop(&mut self) {
        self.remove_reference();
    }
}

unsafe impl<DropFn: FnOnce(*mut u8, Layout)> TaskEventHandler for RcTaskHandler<DropFn> {
    fn on_task_poll(&self) {}
    fn on_waker_wake(&self) {}
    fn on_waker_wake_by_ref(&self) {}

    fn on_waker_clone(&self) {
        self.add_reference();
    }

    fn on_waker_drop(&self) {
        self.remove_reference();
    }
}