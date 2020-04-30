use super::{
    super::{Platform, Worker},
    Task,
    TaskEvent,
};
use core::{
    ptr::NonNull,
    cell::UnsafeCell,
};

pub(crate) struct TaskHeader<P: Platform> {
    task: UnsafeCell<Task<P>>,
    header_offset: usize,
    worker: NonNull<Worker<P>>,
    dispatch: unsafe fn(&Self, TaskEvent),
}

impl<P: Platform> TaskHeader<P> {
    fn new(dispatch: unsafe fn(&Self, TaskEvent)) -> Self {
        Self {
            task: UnsafeCell::new(Task::new(Self::resume)),
            header_offset: 0,
            worker: NonNull::dangling(),
            dispatch,
        }
    }

    unsafe fn resume(task: &Task, worker: Pin<&Worker<P>>) {
        let stub = Self::new(|_, _| {});
        let base_ptr = &stub as *const _ as usize;
        let field_ptr = &stub.task as *const _ as usize;
        let task_offset = field_ptr - base_ptr;

        let task_ptr = task as *const _ as usize;
        let header = &mut *((task_ptr - task_offset) as *mut Self);
        
        header.worker = NonNull::from(worker.into_inner_unchecked());
        header.dispatch(header, TaskEvent::Poll);
        header.worker = NonNull::dangling();
    }

    const RAW_WAKER_VTABLE: RawWakerVTable = RawWakerVTable::new(
        |ptr| unsafe {
            let header = &*(ptr as *const Self);
            header.dispatch(header, TaskEvent::Clone);
            RawWaker::from(ptr, &Self::RAW_WAKER_VTABLE)
        },
        |ptr| unsafe {
            let header = &*(ptr as *const Self);
            header.dispatch(header, TaskEvent::Wake);
        },
        |ptr| unsafe {
            let header = &*(ptr as *const Self);
            header.dispatch(header, TaskEvent::WakeByRef);
        },
        |ptr| unsafe {
            let header = &*(ptr as *const Self);
            header.dispatch(header, TaskEvent::Drop);
        },
    );

    unsafe fn waker(&self) -> Waker {
        let vtable_ptr = &Self::RAW_WAKER_VTABLE;
        let raw_waker = RawWaker::new(self as *const (), vtable_ptr);
        Waker::from_raw(raw_waker)
    }

    unsafe fn from_waker(waker: &Waker) -> Option<NonNull<Self>> {
        let vtable_ptr = &Self::RAW_WAKER_VTABLE as *const _ as usize;

        assert_eq!(mem::size_of::<Waker>(), mem::size_of::<[usize; 2]>());
        let [ptr1, ptr2] = ptr::read(waker as *const _ as *const [usize; 2]);
        if ptr1 != vtable_ptr && ptr2 != vtable_ptr {
            return None;
        }

        let header_ptr = if ptr1 == vtable_ptr { ptr2 } else { ptr1 };
        Some(NonNull::new_unchecked(header_ptr as *mut Self))
    }
}