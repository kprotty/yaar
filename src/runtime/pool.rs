use super::{
    builder::Builder,
    idle::{IdleNode, IdleNodeProvider, IdleQueue},
    io::IoDriver,
    task::Task,
    worker::Worker,
};
use std::{
    mem,
    num::NonZeroUsize,
    pin::Pin,
    ptr::NonNull,
    sync::{
        atomic::{AtomicUsize, AtomicBool, Ordering},
        Arc,
    },
};

#[allow(unused)]
#[derive(Debug)]
pub(crate) enum PoolEvent {
    TaskSpawned {
        worker_index: usize,
        task: NonNull<Task>,
    },
    TaskIdling {
        worker_index: usize,
        task: NonNull<Task>,
    },
    TaskScheduled {
        worker_index: usize,
        task: NonNull<Task>,
    },
    TaskPolling {
        worker_index: usize,
        task: NonNull<Task>,
    },
    TaskPolled {
        worker_index: usize,
        task: NonNull<Task>,
    },
    TaskShutdown {
        worker_index: usize,
        task: NonNull<Task>,
    },
    WorkerSpawned {
        worker_index: usize,
    },
    WorkerPushed {
        worker_index: usize,
        task: NonNull<Task>,
    },
    WorkerPopped {
        worker_index: usize,
        task: NonNull<Task>,
    },
    WorkerStole {
        worker_index: usize,
        target_index: usize,
        count: usize,
    },
    WorkerIdling {
        worker_index: usize,
    },
    WorkerScheduled {
        worker_index: usize,
    },
    WorkerShutdown {
        worker_index: usize,
    },
}

#[derive(Copy, Clone, Debug)]
struct Sync {
    idle: usize,
    spawned: usize,
    searching: usize,
}

impl Sync {
    const BITS: u32 = usize::BITS / 3;
    const MASK: usize = (1 << Self::BITS) - 1;

    const IDLE_SHIFT: u32 = Self::BITS * 0;
    const SPAWN_SHIFT: u32 = Self::BITS * 1;
    const SEARCH_SHIFT: u32 = Self::BITS * 2;
}

impl Into<usize> for Sync {
    fn into(self) -> usize {
        assert!(self.spawned <= Self::MASK);
        assert!(self.idle <= self.spawned);
        assert!(self.searching <= self.spawned);

        (self.idle << Self::IDLE_SHIFT)
            | (self.spawned << Self::SPAWN_SHIFT)
            | (self.searching << Self::SEARCH_SHIFT)
    }
}

impl From<usize> for Sync {
    fn from(value: usize) -> Self {
        Self {
            idle: (value >> Self::IDLE_SHIFT) & Self::MASK,
            spawned: (value >> Self::SPAWN_SHIFT) & Self::MASK,
            searching: (value >> Self::SEARCH_SHIFT) & Self::MASK,
        }
    }
}

#[repr(align(8))]
pub struct Pool {
    sync: AtomicUsize,
    notified: AtomicBool,
    pending: AtomicUsize,
    injecting: AtomicUsize,
    idle_queue: IdleQueue,
    stack_size: Option<NonZeroUsize>,
    pub io_driver: Arc<IoDriver>,
    pub workers: Pin<Box<[Worker]>>,
}

impl<'a> IdleNodeProvider for &'a Pool {
    fn with<T>(&self, index: usize, f: impl FnOnce(Pin<&IdleNode>) -> T) -> T {
        f(unsafe { Pin::new_unchecked(&self.workers()[index].idle_node) })
    }
}

impl Pool {
    pub fn from_builder(builder: &Builder) -> Arc<Pool> {
        let num_threads = builder
            .max_threads
            .unwrap_or_else(|| num_cpus::get())
            .min(Sync::MASK)
            .max(1);

        let stack_size = builder.stack_size.and_then(NonZeroUsize::new);

        Arc::new(Self {
            sync: AtomicUsize::new(0),
            notified: AtomicBool::new(false),
            pending: AtomicUsize::new(0),
            injecting: AtomicUsize::new(0),
            idle_queue: IdleQueue::default(),
            stack_size: stack_size,
            io_driver: Arc::new(IoDriver::default()),
            workers: (0..num_threads)
                .map(|_| Worker::default())
                .collect::<Box<[Worker]>>()
                .into(),
        })
    }

    pub fn workers(&self) -> &[Worker] {
        &self.workers[..]
    }

    pub(crate) fn emit(&self, event: PoolEvent) {
        // TODO: Add custom tracing/handling here
        mem::drop(event)
    }

    pub fn next_inject_index(&self) -> usize {
        self.injecting.fetch_add(1, Ordering::Relaxed) % self.workers.len()
    }

    pub fn mark_task_begin(&self) {
        let pending = self.pending.fetch_add(1, Ordering::Relaxed);
        assert_ne!(pending, usize::MAX);
    }

    pub fn mark_task_end(&self) {
        let pending = self.pending.fetch_sub(1, Ordering::AcqRel);
        assert_ne!(pending, 0);

        if pending == 1 {
            self.io_driver.notify();
            self.idle_queue.shutdown(self);
        }
    }

    #[cold]
    pub fn notify(self: &Arc<Self>) {
        self.sync
            .fetch_update(Ordering::Release, Ordering::Relaxed, |sync| {
                let mut sync: Sync = sync.into();
                if sync.searching > 0 {
                    return None;
                }

                if sync.idle == 0 {
                    if sync.spawned < self.workers.len() {
                        sync.spawned += 1;
                    } else {
                        return None;
                    }
                }

                sync.searching = 1;
                Some(sync.into())
            })
            .map(|sync| {
                assert!(!self.notified.load(Ordering::Relaxed));
                self.notified.store(true, Ordering::Release);

                let sync: Sync = sync.into();
                if sync.idle > 0 {
                    let notify_io = sync.idle == 1;
                    self.unpark(notify_io);
                    return;
                }

                assert!(sync.spawned < self.workers.len());
                let worker_index = sync.spawned;
                let pool = Arc::clone(self);

                // Run the first worker using the caller's thread
                if worker_index == 0 {
                    return pool.with_worker(worker_index);
                }

                // Create a ThreadBuilder to spawn a worker thread
                let mut builder = std::thread::Builder::new().name(String::from("yaar-thread"));
                if let Some(stack_size) = self.stack_size {
                    builder = builder.stack_size(stack_size.get());
                }

                builder
                    .spawn(move || pool.with_worker(worker_index))
                    .expect("Failed to spawn a worker thread");
            })
            .unwrap_or(())
    }

    pub fn discovered(&self, _index: usize, is_searching: &mut bool) -> bool {
        if !*is_searching {
            return false;
        }
        
        let update: usize = 1 << Sync::SEARCH_SHIFT;
        let sync: Sync = self
            .sync
            .fetch_sub(update, Ordering::SeqCst)
            .into();
        
        *is_searching = false;
        assert_ne!(sync.searching, 0);
        sync.searching == 1
    }

    pub fn try_search(&self, _index: usize, is_searching: &mut bool) -> bool {
        if *is_searching {
            return true;
        }

        let mut sync: Sync = self.sync.load(Ordering::Relaxed).into();
        if 2 * sync.searching >= self.workers.len() {
            return false;
        }

        sync = self
            .sync
            .fetch_add(1 << Sync::SEARCH_SHIFT, Ordering::Acquire)
            .into();

        assert_ne!(sync.searching, sync.spawned);
        *is_searching = true;
        true
    }

    #[cold]
    pub fn wait(
        self: &Arc<Self>,
        index: usize,
        is_searching: &mut bool,
        has_pending_tasks: impl FnOnce() -> bool,
    ) -> bool {
        let mut update: usize = 1 << Sync::IDLE_SHIFT;
        if *is_searching {
            update = update.wrapping_sub(1 << Sync::SEARCH_SHIFT);
        }

        let mut sync: Sync = self
            .sync
            .fetch_add(update, Ordering::AcqRel)
            .into();

        assert_ne!(sync.idle, sync.spawned);
        if *is_searching {
            assert_ne!(sync.searching, 0);
        }

        self.emit(PoolEvent::WorkerIdling {
            worker_index: index,
        });

        let has_pending = *is_searching && {
            *is_searching = false;
            sync.searching == 1 && has_pending_tasks()
        };

        update = 1 << Sync::IDLE_SHIFT;
        if has_pending {
            update = update.wrapping_add(1 << Sync::SEARCH_SHIFT);
        } else {
            self.park(index, is_searching);
        }
        
        sync = self
            .sync
            .fetch_sub(update, Ordering::Relaxed)
            .into();

        assert_ne!(sync.idle, 0);
        if has_pending {
            *is_searching = true;
            assert_ne!(sync.searching, sync.spawned);
        }

        self.emit(PoolEvent::WorkerScheduled {
            worker_index: index,
        });

        self.pending.load(Ordering::Relaxed) > 0
    }

    #[cold]
    fn park(&self, index: usize, is_searching: &mut bool) {
        loop {
            if self.pending.load(Ordering::Acquire) == 0 {
                return;
            }

            if self.notified.load(Ordering::Relaxed) {
                if self.notified.swap(false, Ordering::Acquire) {
                    *is_searching = true;
                    return;
                }
            }

            if self.io_driver.poll(None) {
                continue;
            }

            self.idle_queue.wait(self, index, || {
                !self.notified.load(Ordering::Relaxed)
            });
        }
    }

    #[cold]
    fn unpark(&self, notify_io: bool) {
        if self.idle_queue.signal(self) {
            return;
        }

        if notify_io {
            self.io_driver.notify();
        }
    }
}
