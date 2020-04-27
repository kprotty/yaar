use super::{
    Worker,
    Platform,
    Scheduler,
};
use core::{
    cell::Cell,
    num::NonZeroUsize,
    ptr::{null_mut, NonNull},
    slice::from_raw_parts,
    mem::align_of,
    sync::atomic::{Ordering, AtomicPtr},
};

pub struct Node<P: Platform> {
    pub data: P::NodeData,
    pub scheduler: NonNull<Scheduler<P>>,
    pub(crate) next: Cell<Option<NonNull<Self>>>,
    workers_ptr: NonNull<NonNull<Worker<P>>>,
    workers_len: NonZeroUsize,
    idle_workers: AtomicPtr<Worker<P>>,
}

impl<P: Platform> Node<P> {
    pub unsafe fn init(
        &mut self,
        scheduler: NonNull<Scheduler<P>>,
        workers: &[NonNull<Worker<P>>],
        data: P::NodeData,
    ) {
        *self = Self {
            data,
            scheduler,
            next: Cell::new(None),
            workers_ptr: NonNull::new_unchecked(workers.as_ptr() as *mut _),
            workers_len: NonZeroUsize::new_unchecked(workers.len()),
            idle_workers: AtomicPtr::new(null_mut()),
        };

        for worker in workers.iter() {
            let worker_ref = worker.as_ref();
            worker_ref.node.set(NonNull::from(self));
            worker_ref.next.set(NonNull::new(*self.idle_workers.get_mut()));
            *self.idle_workers.get_mut() = worker.as_ptr();
        }
    }

    #[inline]
    pub fn workers(&self) -> &[NonNull<Worker<P>>] {
        from_raw_parts(
            self.workers_ptr.as_ptr(),
            self.workers_len.get(),
        )
    }

    pub(crate) unsafe fn put_idle_worker(&self, worker: NonNull<Worker<P>>) -> bool {
        assert!(align_of::<Worker<P>>() > 0x1);
        let mut head = self.idle_workers.load(Ordering::Relaxed);

        loop {
            let new_head = match (head as usize) {
                0x1 => null_mut(),
                _ => {
                    worker.as_ref().next.set(NonNull::new(head));
                    worker.as_ptr()
                },
            };

            match self.idle_workers.compare_exchange_weak(
                head,
                new_head,
                Ordering::Release,
                Ordering::Relaxed,
            ) {
                Ok(0x1) => return false,
                Ok(_) => return true,
                Err(e) => head = e,
            }
        }
    }

    pub(crate) unsafe fn find_idle_worker(&self) -> Option<NonNull<Worker<P>>> {
        assert!(align_of::<Worker<P>>() > 0x1);
        let mut head = self.idle_workers.load(Ordering::Acquire);

        loop {
            let new_head = match (head as usize) {
                0x0 => 0x1 as *mut _,
                0x1 => return None,
                _ => (*head).next.get().map(|p| p.as_ptr()).unwrap_or(null_mut()),
            };

            match self.idle_workers.compare_exchange_weak(
                head,
                new_head,
                Ordering::Acquire,
                Ordering::Acquire,
            ) {
                Ok(0x0) => return None,
                Ok(_) => return Some(NonNull::new_unchecked(head)),
                Err(e) => head = e, 
            }
        }
    }
}