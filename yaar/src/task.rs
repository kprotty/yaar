use core::{
    cell::UnsafeCell,
    num::NonZeroUsize,
    ptr::{self, NonNull},
    pin::Pin,
    marker::PhantomPinned,
    mem::{self, MaybeUninit},
    future::Future,
    task::{Waker, RawWaker, RawWakerVTable, Context, Poll},
    sync::atomic::{spin_loop_hint, fence, Ordering, AtomicUsize, AtomicPtr, AtomicBool},
};
use yaar_lock::utils::{CachePadded, UnwrapUnchecked, unreachable, Unsync};

pub trait Platform {

}







pub struct Worker<P: Platform> {
    _pinned: PhantomPinned,
    node: NonNull<Node<P>>,
    next: UnsafeCell<Option<NonNull<Self>>>,
    local_list: 
}

struct GlobalList {
    _pinned: PhantomPinned,
    tail: CachePadded<AtomicPtr<Runnable>>,
    is_locked: CachePadded<AtomicBool>,
    list: UnsafeCell<List>,
    stub: UnsafeCell<Runnable>,
}

impl GlobalList {
    fn init(self: Pin<&mut Self>) {
        let mut_self = unsafe { self.get_unchecked_mut() };
        let stub_ptr = mut_self.stub.get();
        *mut_self = Self {
            _pinned: PhantomPinned,
            tail: CachePadded::new(AtomicPtr::new(stub_ptr)),
            is_locked: CachePadded::new(AtomicBool::new(false)),
            list: UnsafeCell::new(List::default()),
            stub: UnsafeCell::new(Runnable::new(
                Locality::Worker,
                Priority::Low,
                |_| unreachable!("GlobalList stub was ran"),
            )),
        };
    }

    #[inline]
    fn try_with_list<T>(
        &self,
        f: impl FnOnce(&mut List) -> Option<T>,
    ) -> Option<T> {
        if !self.is_locked.load(Ordering::Relaxed) {
            if !self.is_locked.compare_and_swap(false, true, Ordering::Acquire) {
                let result = f(unsafe { &mut *self.list.get() });
                self.is_locked.store(false, Ordering::Release);
                return result;
            }
        }

        None
    }

    #[inline]
    unsafe fn push(&self, list: &List) {
        let front = list.head;
        let back = match list.tail {
            Some(tail) => tail,
            None => return,
        };

        fence(Ordering::Release);
        let prev = self.tail.swap(back.as_ptr(), Ordering::AcqRel);
        debug_assert!(!prev.is_null());
        (&*prev).store_next(front, Ordering::Release);
    }

    unsafe fn pop(
        &self,
        local_list: &LocalList,
        max: NonZeroUsize,
    ) -> Option<NonNull<Runnable>> {
        self.try_with_list(|list| {
            let mut first = None;


            for i in 0..(tail.wrapping_sub(head) + 1).min(max.get()) {
                // TODO: impl global queue alg from worker.rs
            }

            first
        })
    }
}

struct LocalList {
    head: CachePadded<AtomicUsize>,
    tail: CachePadded<AtomicUsize>,
    buffer: MaybeUninit<[NonNull<Runnable>; Self::SIZE]>,
}

impl LocalList {
    const SIZE: usize = 128;

    fn init(&mut self) {
        *self = Self {
            head: CachePadded::new(AtomicUsize::new(0)),
            tail: CachePadded::new(AtomicUsize::new(0)),
            buffer: unsafe { MaybeUninit::uninit() },
        };
    }

    unsafe fn read(&self, index: usize) -> NonNull<Runnable> {
        let ptr = self.buffer.as_ptr() as *const NonNull<Runnable>;
        ptr::read(ptr.add(index % Self::SIZE))
    }

    unsafe fn write(&self, index: usize, value: NonNull<Runnable>) {
        let ptr = self.buffer.as_ptr() as *mut NonNull<Runnable>;
        ptr::write(ptr.add(index % Self::SIZE), value)
    }

    unsafe fn pop(&self) -> Option<NonNull<Runnable>> {
        let mut head = self.head.load(Ordering::Relaxed);
        let tail = self.tail.load_unsync();

        while tail.wrapping_sub(head) != 0 {
            let runnable = self.read(head);
            match self.head.compare_exchange_weak(
                head,
                head.wrapping_add(1),
                Ordering::Relaxed,
                Ordering::Relaxed,
            ) {
                Ok(_) => return Some(runnable),
                Err(e) => head = e,
            }
        }

        None
    }

    unsafe fn steal_from(&self, target: &LocalList) -> Option<NonNull<Runnable>> {
        let head = self.head.load(Ordering::Relaxed);
        let tail = self.tail.load_unsync();
        let size = tail.wrapping_sub(head);

        loop {
            let target_head = target.head.load(Ordering::Acquire);
            let target_tail = target.tail.load(Ordering::Acquire);

            let steal_size = target_tail.wrapping_sub(target_head);
            let steal_size = NonZeroUsize::new_unchecked(match steal_size {
                0 => return None,
                1 => 1,
                target_size => (target_size >> 1).min(size),
            });

            for i in 0..steal_size.get() {
                let runnable = target.read(target_head.wrapping_add(i));
                self.write(tail.wrapping_add(i), runnable);
            }

            if let Err(_) = target.head.compare_exchange_weak(
                target_head,
                target_head.wrapping_add(steal_size.get()),
                Ordering::Relaxed,
                Ordering::Relaxed,
            ) {
                spin_loop_hint();
                continue;
            }

            let size = steal_size.get() - 1;
            if let Some(size) = NonZeroUsize::new(size) {
                self.tail.store(tail.wrapping_add(size), Ordering::Release);
            }

            let runnable = self.read(tail.wrapping_add(steal_size.get()));
            return Some(runnable);
        }
    }
}

#[derive(Default, Debug)]
pub struct List<P: Platform> {
    head: Option<NonNull<Runnable<P>>>,
    tail: Option<NonNull<Runnable<P>>>,
}

impl<'_, P: Platform> From<Pin<&'_ mut Runnable<P>>> for List<P> {
    fn from(runnable: Pin<&'_ mut Runnable>) -> Self {
        let mut list = List::default();
        list.push_front(runnable);
        list
    }
}

impl List {
    pub unsafe fn push_front(&mut self, runnable: Pin<&mut Runnable>) {
        let runnable = runnable.into_inner_unchecked();
        runnable.set_next(self.head);
        let runnable = Some(NonNull::from(runnable));

        if mem::replace(&mut self.head, runnable).is_none() {
            self.tail = runnable;
        }
    }

    pub unsafe fn push_back(&mut self, runnable: Pin<&mut Runnable>) {
        let runnable = runnable.into_inner_unchecked();
        runnable.set_next(None);
        let runnable = Some(NonNull::from(runnable));

        if let Some(old_tail) = mem::replace(&mut self.tail, runnable) {
            old_tail.as_mut().set_next(runnable);
        } else {
            self.head = runnable;
        }
    }

    pub unsafe fn pop(&mut self) -> Option<NonNull<Runnable>> {
        self.head.map(|runnable| {
            self.head = runnable.as_ref().next();
            if self.head.is_none() {
                self.tail = None;
            }
            runnable
        })
    }
}



struct TaskHeader<P> {
    runnable: Runnable
}

pub fn current_worker<P: Platform>() -> impl Future<Output = Option<NonNull<Worker<P>>>> {
    struct GetWorker<P: Platform>;

    impl<P: Platform> Future for GetWorker<P> {
        type Output = Option<NonNull<Worker<P>>>;

        fn poll(self: Pin<&mut Self>, ctx: &mut Context<'_>) -> Self::Output {
            unsafe {
                let raw = 
            }
        }
    }

    GetWorker{}
}

pub struct RunnableContext<P: Platform> {
    runnable: NonNull<Runnable>,
    worker: NonNull<Worker<P>>,
}

impl<P: Platform> RunnableContext<P> {
    pub const fn new(
        runnable: Pin<&mut Runnable>,
        worker: Pin<&Worker<P>>,
    ) -> Self {
        unsafe {
            Self {
                runnable: NonNull::from(runnable.into_inner_unchecked()),
                worker: NonNull::from(worker.into_inner_unchecked()),
            }
        }
    }

    pub unsafe fn poll<F: Future>(
        &mut self,
        future: Pin<&mut F>,
    ) -> 
}
