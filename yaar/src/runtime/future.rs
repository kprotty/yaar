use super::{
    super::{Platform, Worker},
    Task,
    TaskResumeFn,
};

// TODO: move this all inot runtime since that handles futures based stuff 
use core::{
    future::Future,
    ptr::{self, NonNull},
    mem::MaybeUninit,
    task::{Waker, Context, Poll},
    sync::atomic::{Ordering, AtomicUsize},
};

#[cfg(not(feature = "std"))]
type TaskFutureError = ();

#[cfg(feature = "std")]
type TaskFutureError = Box<dyn std::any::Any + Sized + 'static>;

#[repr(C, usize)]
enum FutureState<F: Future> {
    Pending(F),
    Ready(F::Output),
    Error(TaskFutureError),
}


#[repr(C)]
struct Header<P: Platform> {
    task: Task<P>,
    ref_count: AtomicUsize,
    state: AtomicUsize,
}

impl<P: Platform> Header<P> {
    #[inline]
    fn payload<T>(&self) -> NonNull<T> {
        unsafe {
            let payload_ptr = (self as *const Self).add(1);
            NonNull::new_unchecked(payload_ptr as *mut T)
        }    
    }
}

#[repr(C)]
pub(crate) struct TaskFuture<P: Platform, F: Future> {
    task: Task<P>,
    ref_count: AtomicUsize,
    future_state: FutureState<F>,
}

impl<P: Platform, F: Future> TaskFuture<P, F> {
    const VTABLE: VTable = VTable {
        resume: Self::resume,
    };

    fn new(future: F) {
        Self {
            task: Task::new(NonNull::from(&VTABLE.resume)),
            ref_count: AtomicUsize::new(0),
            future_state: FutureState::Pending(future),
        }
    }

    #[inline]
    fn header(&self) -> NonNull<Header<P>> {
        unsafe {
            let header_ptr = (self as *const _ as *mut Header<P>);
            NonNull::new_unchecked(header_ptr)
        }
    }
}

struct PollContext<P: Platform> {

}