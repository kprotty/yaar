use super::{GlobalQueue, Platform, Scheduler, TaggedWorker, Thread, ThreadState, Worker};
use core::{
    cell::Cell,
    ptr::{null, NonNull},
    slice::from_raw_parts,
    sync::atomic::{compiler_fence, AtomicUsize, Ordering},
};
use crossbeam_utils::CachePadded;
use lock_api::Mutex;
use yaar_lock::ThreadEvent;

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
    #[inline]
    pub fn workers(&self) -> &[Worker<P>] {
        unsafe { from_raw_parts(self.workers_ptr.as_ptr(), self.workers_len) }
    }

    #[inline]
    pub fn scheduler(&self) -> Option<&Scheduler<P>> {
        self.scheduler.get().map(|ptr| unsafe { &*ptr.as_ptr() })
    }

    pub(crate) fn spawn_worker(&self) {
        // Should only spawn a worker if there aren't any currently searching for work
        // since they would steal the tasks meant for any newly spawned worker. This
        // also acts as a method to avoid thundering herd of many workers being spawned
        // at once to start sealing from each other, only to possibly go back into idle
        // state.
        if self
            .workers_searching
            .compare_exchange(0, 1, Ordering::Acquire, Ordering::Relaxed)
            .is_err()
        {
            return;
        }

        {
            let mut pool = self.pool.lock();
            if let Some(worker) = pool.find_worker(self) {
                // Try to run the worker on an existing thread
                if let Some(thread) = pool.find_thread(self, worker) {
                    let new_state = TaggedWorker::new(worker, ThreadState::Searching);
                    thread
                        .worker_state
                        .store(new_state.into(), Ordering::Relaxed);
                    thread.event.set();
                    return;
                // Try to spawn a new thread on the platform to run the worker
                } else if pool.free_threads != 0 {
                    let scheduler = self
                        .scheduler()
                        .expect("spawn_worker: Node without scheduler");
                    if scheduler.platform().spawn_thread(worker, Thread::<P>::run) {
                        scheduler.active_threads.fetch_add(1, Ordering::Relaxed);
                        pool.free_threads -= 1;
                        return;
                    }
                }
                // Failed to run the worker on a thread, put it back into the pool
                pool.put_worker(self, worker);
            }
        }

        // Failed to run a new worker, reset the spinning count incremented from
        // earlier.
        let workers_searching = self.workers_searching.fetch_sub(1, Ordering::Relaxed);
        debug_assert!(workers_searching >= 1 && workers_searching < self.workers().len());
    }
}

pub(crate) struct Pool<P: Platform> {
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

    pub fn put_thread(&mut self, _node: &Node<P>, thread: &Thread<P>) {
        let thread_ptr = NonNull::new(thread as *const _ as *mut _);
        if let Some(next) = self.idle_threads {
            unsafe { next.as_ref().prev.set(thread_ptr.clone()) };
        }

        thread.prev.set(None);
        thread.next.set(self.idle_threads);
        self.idle_threads = thread_ptr;

        let new_state = TaggedWorker::<P>::new(null(), ThreadState::Idle);
        thread
            .worker_state
            .store(new_state.into(), Ordering::Relaxed);
        compiler_fence(Ordering::Release);
    }

    pub fn find_thread<'a>(
        &mut self,
        _node: &Node<P>,
        worker: &Worker<P>,
    ) -> Option<&'a Thread<P>> {
        // Try to run the worker on its previous thread if any
        if let Some(thread_ptr) = worker.last_thread.get() {
            unsafe {
                let thread = &*thread_ptr.as_ptr();
                // Only acquire the thread if it's idle
                if thread
                    .worker_state
                    .compare_exchange(
                        TaggedWorker::<P>::new(null(), ThreadState::Idle).into(),
                        TaggedWorker::<P>::new(worker, ThreadState::Searching).into(),
                        Ordering::Release,
                        Ordering::Relaxed,
                    )
                    .is_ok()
                {
                    let prev = thread.prev.get();
                    let next = thread.next.get();
                    if let Some(prev) = prev {
                        prev.as_ref().next.set(next);
                    }
                    if let Some(next) = next {
                        next.as_ref().prev.set(prev);
                    }
                    if prev.is_none() {
                        self.idle_threads = next;
                    }
                    
                    thread.prev.set(None);
                    thread.next.set(None);
                    compiler_fence(Ordering::Release);
                    return Some(thread);
                }
            }
        }

        // Check if there's a free thread in the idle thread list
        self.idle_threads.map(|thread_ptr| unsafe {
            let thread = &*thread_ptr.as_ptr();
            self.idle_threads = thread.next.get();
            if let Some(next) = self.idle_threads {
                next.as_ref().prev.set(None);
            }
            
            thread.next.set(None);
            compiler_fence(Ordering::Release);
            thread
        })
    }
}
