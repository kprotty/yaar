use super::{
    super::sync::low_level::ThreadLocal,
    idle::IdleNode,
    pool::{Pool, PoolEvent},
    queue::{Buffer, Injector, List, Popped},
    task::Task,
};
use std::{
    ffi::c_void, marker::PhantomPinned, mem, pin::Pin, ptr::NonNull, sync::Arc, time::Duration,
};

#[derive(Default)]
pub struct Worker {
    buffer: Buffer,
    injector: Injector,
    pub idle_node: IdleNode,
}

struct WorkerRef {
    pool: Arc<Pool>,
    index: usize,
    _pinned: PhantomPinned,
}

impl Pool {
    fn tls_pool_ref() -> &'static ThreadLocal {
        static TLS_POOL_REF: ThreadLocal = ThreadLocal::new();
        &TLS_POOL_REF
    }

    pub fn with_worker(self: Arc<Self>, index: usize) {
        unsafe {
            let worker_ref = WorkerRef {
                pool: self,
                index,
                _pinned: PhantomPinned,
            };

            let worker_ref = Pin::new_unchecked(&worker_ref);
            let old_ptr = Self::tls_pool_ref()
                .with(|ptr| mem::replace(ptr, &*worker_ref as *const WorkerRef as *const c_void));

            worker_ref.pool.run(index);
            Self::tls_pool_ref().with(|ptr| *ptr = old_ptr);
        }
    }

    pub fn with_current<T>(f: impl FnOnce(&Arc<Self>, usize) -> T) -> Option<T> {
        Self::tls_pool_ref().with(|ptr| {
            NonNull::new(*ptr as *const WorkerRef as *mut WorkerRef).map(|worker_ref| unsafe {
                let worker_ref = worker_ref.as_ref();
                f(&worker_ref.pool, worker_ref.index)
            })
        })
    }

    pub fn run(self: &Arc<Self>, index: usize) {
        let mut tick: usize = 0;
        let mut is_searching = false;
        let mut xorshift = 0xdeadbeef + index;

        self.emit(PoolEvent::WorkerSpawned {
            worker_index: index,
        });

        while let Some(popped) = {
            let be_fair = tick % 64 == 0;
            self.pop(index, be_fair, &mut xorshift, &mut is_searching)
        } {
            if self.discovered(index, &mut is_searching) || popped.pushed > 0 {
                self.notify();
            }

            let task = popped.task;
            self.emit(PoolEvent::TaskScheduled {
                worker_index: index,
                task,
            });

            tick = tick.wrapping_add(1);
            unsafe {
                let vtable = task.as_ref().vtable;
                (vtable.poll_fn)(task, self, index)
            }
        }

        self.emit(PoolEvent::WorkerShutdown {
            worker_index: index,
        });
    }

    pub(crate) unsafe fn push(
        self: &Arc<Self>,
        index: Option<usize>,
        task: NonNull<Task>,
        mut be_fair: bool,
    ) {
        let workers = self.workers();
        let index = index.unwrap_or_else(|| {
            be_fair = true;
            self.next_inject_index()
        });

        let injector = Pin::new_unchecked(&workers[index].injector);
        if be_fair {
            injector.push(List {
                head: task,
                tail: task,
            });
        } else {
            workers[index].buffer.push(task, injector);
        }

        self.emit(PoolEvent::WorkerPushed {
            worker_index: index,
            task,
        });

        self.notify()
    }

    fn pop(
        self: &Arc<Self>,
        index: usize,
        be_fair: bool,
        xorshift: &mut usize,
        is_searching: &mut bool,
    ) -> Option<Popped> {
        if be_fair {
            if let Some(popped) = self.pop_injector(index) {
                return Some(popped);
            }
        }

        if let Some(popped) = self.pop_local(index) {
            return Some(popped);
        }

        if self.io_driver.poll(Some(Duration::ZERO)) {
            if let Some(popped) = self.pop_local(index) {
                return Some(popped);
            }
        }

        self.pop_search(index, xorshift, is_searching)
    }

    fn pop_injector(self: &Arc<Self>, index: usize) -> Option<Popped> {
        self.pop_consume(index, index)
    }

    fn pop_local(self: &Arc<Self>, index: usize) -> Option<Popped> {
        // TODO: add worker-local injector consume here

        self.workers()[index]
            .buffer
            .pop()
            .map(|popped| {
                self.emit(PoolEvent::WorkerPopped {
                    worker_index: index,
                    task: popped.task,
                });

                popped
            })
            .or_else(|| self.pop_injector(index))
    }

    #[cold]
    fn pop_search(
        self: &Arc<Self>,
        index: usize,
        xorshift: &mut usize,
        is_searching: &mut bool,
    ) -> Option<Popped> {
        loop {
            if self.try_search(index, is_searching) {
                if let Some(popped) = self.pop_shared(index, xorshift) {
                    return Some(popped);
                }
            }

            if !self.wait(index, is_searching, || self.has_shared()) {
                return None;
            }
        }
    }

    fn has_shared(self: &Arc<Self>) -> bool {
        self.workers()
            .iter()
            .map(|worker| worker.injector.consumable() || worker.buffer.stealable())
            .filter(|&poppable| !poppable)
            .next()
            .unwrap_or(false)
    }

    #[cold]
    fn pop_shared(self: &Arc<Self>, index: usize, xorshift: &mut usize) -> Option<Popped> {
        let shifts = match usize::BITS {
            32 => (13, 17, 5),
            64 => (13, 7, 17),
            _ => unreachable!("architecture unsupported"),
        };

        let mut rng = *xorshift;
        rng ^= rng << shifts.0;
        rng ^= rng >> shifts.1;
        rng ^= rng << shifts.2;
        *xorshift = rng;

        let num_workers = self.workers().len();
        (0..num_workers)
            .cycle()
            .skip(rng % num_workers)
            .take(num_workers)
            .map(|steal_index| {
                self.pop_consume(index, steal_index)
                    .or_else(|| match steal_index {
                        _ if steal_index == index => None,
                        _ => self.pop_steal(index, steal_index),
                    })
            })
            .filter_map(|popped| popped)
            .next()
    }

    #[cold]
    fn pop_consume(self: &Arc<Self>, index: usize, target_index: usize) -> Option<Popped> {
        self.workers()[index]
            .buffer
            .consume(unsafe { Pin::new_unchecked(&self.workers()[target_index].injector) })
            .map(|popped| {
                self.emit(PoolEvent::WorkerStole {
                    worker_index: index,
                    target_index,
                    count: popped.pushed + 1,
                });

                self.emit(PoolEvent::WorkerPopped {
                    worker_index: index,
                    task: popped.task,
                });

                popped
            })
    }

    #[cold]
    fn pop_steal(self: &Arc<Self>, index: usize, target_index: usize) -> Option<Popped> {
        assert_ne!(index, target_index);
        self.workers()[index]
            .buffer
            .steal(&self.workers()[target_index].buffer)
            .map(|popped| {
                self.emit(PoolEvent::WorkerStole {
                    worker_index: index,
                    target_index,
                    count: popped.pushed + 1,
                });

                self.emit(PoolEvent::WorkerPopped {
                    worker_index: index,
                    task: popped.task,
                });

                popped
            })
    }
}
