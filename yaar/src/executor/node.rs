use super::{GlobalQueue, Platform, Scheduler, Thread, Worker};
use core::{
    cell::Cell,
    num::NonZeroUsize,
    ptr::NonNull,
    slice::from_raw_parts,
    sync::atomic::{compiler_fence, AtomicUsize, Ordering},
};
use crossbeam_utils::CachePadded;
use lock_api::Mutex;

pub struct Node<P: Platform> {
    pub data: P::NodeLocalData,
    pub(crate) run_queue: GlobalQueue<P::RawMutex>,
    pub(crate) pool: Mutex<P::RawMutex, CachePadded<Pool<P>>>,
    pub(crate) scheduler: Cell<Option<NonNull<Scheduler<P>>>>,

    workers_len: usize,
    workers_ptr: NonNull<Worker<P>>,
    pub(crate) workers_idle: AtomicUsize,
    pub(crate) workers_searching: AtomicUsize,
}

unsafe impl<P: Platform> Sync for Node<P> {}

impl<P: Platform> Node<P> {
    pub fn workers(&self) -> &[Worker<P>] {
        unsafe { from_raw_parts(self.workers_ptr.as_ptr(), self.workers_len) }
    }

    pub fn scheduler(&self) -> Option<&Scheduler<P>> {
        self.scheduler.get().map(|ptr| unsafe { &*ptr.as_ptr() })
    }
}

pub(crate) struct Pool<P: Platform> {
    pub max_threads: NonZeroUsize,
    pub free_threads: usize,
    pub idle_threads: Option<NonNull<Thread<P>>>,
    pub idle_workers: Option<NonNull<Worker<P>>>,
}

impl<P: Platform> Pool<P> {
    fn unsync_load_workers_idle(node: &Node<P>) -> usize {
        unsafe { *(&node.workers_idle as *const _ as *const usize) }
    }

    pub fn put_worker(&mut self, node: &Node<P>, worker: &Worker<P>) {
        worker.next.set(self.idle_workers);
        self.idle_workers = NonNull::new(worker as *const _ as *mut _);

        let workers_idle = Self::unsync_load_workers_idle(node);
        debug_assert!(workers_idle < node.workers().len());
        node.workers_idle.store(workers_idle + 1, Ordering::Relaxed);

        compiler_fence(Ordering::Release);
    }

    pub fn find_worker<'a>(&mut self, node: &Node<P>) -> Option<&'a Worker<P>> {
        self.idle_workers.map(|worker_ptr| {
            let worker = unsafe { &*worker_ptr.as_ptr() };
            self.idle_workers = worker.next.get();

            let workers_idle = Self::unsync_load_workers_idle(node);
            debug_assert!(workers_idle > 0 && workers_idle < node.workers().len());
            node.workers_idle.store(workers_idle - 1, Ordering::Relaxed);

            compiler_fence(Ordering::Release);
            worker
        })
    }
}
