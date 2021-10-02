use super::{
    builder::Builder,
    idle::Semaphore,
    io::IoDriver,
    queue::{Buffer, Injector, List},
    task::Task,
};
use std::{
    cell::RefCell,
    mem,
    num::NonZeroUsize,
    pin::Pin,
    ptr::NonNull,
    rc::Rc,
    sync::{
        atomic::{AtomicUsize, Ordering},
        Arc,
    },
    thread,
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
        worker_index: Option<usize>,
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

#[derive(Copy, Clone, Debug, Eq, PartialEq)]
enum State {
    Pending = 0,
    Signaled = 1,
    Waking = 2,
}

impl From<usize> for State {
    fn from(value: usize) -> Self {
        match value {
            0 => Self::Pending,
            1 => Self::Signaled,
            2 => Self::Waking,
            _ => unreachable!("invalid pool State"),
        }
    }
}

#[derive(Copy, Clone, Debug)]
struct Sync {
    idle: usize,
    spawned: usize,
    notified: bool,
    state: State,
}

impl Sync {
    const COUNT_BITS: u32 = (usize::BITS - 4) / 2;
    const COUNT_MASK: usize = (1 << Self::COUNT_BITS) - 1;
}

impl Into<usize> for Sync {
    fn into(self) -> usize {
        assert!(self.spawned <= Self::COUNT_MASK);
        assert!(self.idle <= self.spawned);

        (self.state as usize)
            | ((self.notified as usize) << 2)
            | (self.spawned << (4 + (Self::COUNT_BITS * 0)))
            | (self.idle << (4 + (Self::COUNT_BITS * 1)))
    }
}

impl From<usize> for Sync {
    fn from(value: usize) -> Self {
        Self {
            idle: (value >> (4 + (Self::COUNT_BITS * 1))) & Self::COUNT_MASK,
            spawned: (value >> (4 + (Self::COUNT_BITS * 0))) & Self::COUNT_MASK,
            notified: value & (1 << 2) != 0,
            state: State::from(value & 0b11),
        }
    }
}

#[derive(Default)]
struct Worker {
    buffer: Buffer,
    injector: Injector,
}

#[repr(align(8))]
pub struct Pool {
    sync: AtomicUsize,
    pending: AtomicUsize,
    injecting: AtomicUsize,
    idle_semaphore: Semaphore,
    stack_size: Option<NonZeroUsize>,
    pub io_driver: Arc<IoDriver>,
    workers: Pin<Box<[Worker]>>,
}

impl Pool {
    pub fn from_builder(builder: &Builder) -> Arc<Pool> {
        let max_threads = builder
            .max_threads
            .unwrap_or_else(|| num_cpus::get())
            .min(Sync::COUNT_MASK)
            .max(1);

        let stack_size = builder.stack_size.and_then(NonZeroUsize::new);

        let pool = Arc::new(Self {
            sync: AtomicUsize::new(0),
            pending: AtomicUsize::new(0),
            injecting: AtomicUsize::new(0),
            idle_semaphore: Semaphore::default(),
            stack_size,
            io_driver: Arc::new(IoDriver::default()),
            workers: (0..(max_threads + 1))
                .map(|_| Worker::default())
                .collect::<Box<[Worker]>>()
                .into(),
        });

        let io_pool = Arc::clone(&pool);
        thread::spawn(move || {
            io_pool.run_worker(max_threads, |p, _i| p.io_driver.poll());
        });

        pool
    }

    pub(crate) fn emit(&self, event: PoolEvent) {
        // TODO: Add custom tracing/handling here
        mem::drop(event)
    }

    pub fn mark_task_begin(&self) {
        let pending = self.pending.fetch_add(1, Ordering::Relaxed);
        assert_ne!(pending, usize::MAX);
    }

    pub fn mark_task_end(&self) {
        let pending = self.pending.fetch_sub(1, Ordering::AcqRel);
        assert_ne!(pending, 0);

        if pending == 1 {
            self.io_driver.shutdown();
            self.workers.iter().for_each(|_| self.idle_semaphore.post());
        }
    }

    pub unsafe fn schedule(
        self: &Arc<Self>,
        index: Option<usize>,
        task: NonNull<Task>,
        mut be_fair: bool,
    ) {
        let index = index.unwrap_or_else(|| {
            be_fair = true;
            self.injecting.fetch_add(1, Ordering::Relaxed) % self.workers.len()
        });

        let injector = Pin::new_unchecked(&self.workers[index].injector);
        if be_fair {
            injector.push(List {
                head: task,
                tail: task,
            });
        } else {
            self.workers[index].buffer.push(task, injector);
        }

        self.emit(PoolEvent::WorkerPushed {
            worker_index: index,
            task,
        });

        let is_waking = false;
        self.notify(is_waking)
    }

    #[cold]
    fn notify(self: &Arc<Self>, is_waking: bool) {
        let max_threads = self.workers.len() - 1;
        self.sync
            .fetch_update(Ordering::Release, Ordering::Relaxed, |sync| {
                let mut sync: Sync = sync.into();
                assert!(sync.idle <= sync.spawned);

                let can_wake = is_waking || sync.state == State::Pending;
                if is_waking {
                    assert_eq!(sync.state, State::Waking);
                }

                if can_wake && sync.idle > 0 {
                    sync.state = State::Signaled;
                } else if can_wake && sync.spawned < max_threads {
                    sync.state = State::Signaled;
                    sync.spawned += 1;
                } else if is_waking {
                    sync.state = State::Pending;
                } else if sync.notified {
                    return None;
                }

                sync.notified = true;
                Some(sync.into())
            })
            .map(|sync| {
                let sync: Sync = sync.into();
                assert!(sync.idle <= sync.spawned);

                let can_wake = is_waking || sync.state == State::Pending;
                if !can_wake {
                    return;
                }

                if sync.idle > 0 {
                    return self.idle_semaphore.post();
                }

                if sync.spawned == max_threads {
                    return;
                }

                assert!(sync.spawned < max_threads);
                let worker_index = sync.spawned;
                let pool = Arc::clone(self);

                // Run the first two workers using the caller's thread
                if worker_index == 0 {
                    return pool.run_worker(worker_index, |p, i| p.run(i));
                }

                // Create a ThreadBuilder to spawn a worker thread
                let mut builder = thread::Builder::new().name(String::from("yaar-thread"));
                if let Some(stack_size) = self.stack_size {
                    builder = builder.stack_size(stack_size.get());
                }

                builder
                    .spawn(move || pool.run_worker(worker_index, |p, i| p.run(i)))
                    .expect("Failed to spawn a worker thread");
            })
            .unwrap_or(())
    }

    #[cold]
    fn wait(self: &Arc<Self>, index: usize, mut is_waking: bool) -> Option<bool> {
        let mut is_idle = false;
        loop {
            let result = self
                .sync
                .fetch_update(Ordering::Acquire, Ordering::Relaxed, |sync| {
                    let mut sync: Sync = sync.into();
                    if is_waking {
                        assert_eq!(sync.state, State::Waking);
                    }

                    if sync.notified {
                        if sync.state == State::Signaled {
                            sync.state = State::Waking;
                        }
                        if is_idle {
                            sync.idle -= 1;
                        }
                    } else if !is_idle {
                        sync.idle += 1;
                        if is_waking {
                            sync.state = State::Pending;
                        }
                    } else {
                        return None;
                    }

                    sync.notified = false;
                    Some(sync.into())
                });

            if let Ok(sync) = result.map(Sync::from) {
                if sync.notified {
                    if is_idle {
                        self.emit(PoolEvent::WorkerScheduled {
                            worker_index: index,
                        });
                    }

                    return Some(is_waking || sync.state == State::Signaled);
                }

                is_idle = true;
                is_waking = false;

                self.emit(PoolEvent::WorkerIdling {
                    worker_index: index,
                });
            }

            if self.pending.load(Ordering::SeqCst) == 0 {
                if is_idle {
                    self.emit(PoolEvent::WorkerScheduled {
                        worker_index: index,
                    });
                }

                return None;
            }

            self.idle_semaphore.wait();
            continue;
        }
    }

    fn tls_pool_ref() -> &'static thread::LocalKey<RefCell<Option<Rc<(Arc<Self>, usize)>>>> {
        thread_local!(static TLS_POOL_REF: RefCell<Option<Rc<(Arc<Pool>, usize)>>> = RefCell::new(None));
        &TLS_POOL_REF
    }

    pub fn with_current<T>(f: impl FnOnce(&Arc<Self>, usize) -> T) -> Option<T> {
        Self::tls_pool_ref()
            .with(|rc| rc.borrow().as_ref().map(Rc::clone))
            .map(|pool_ref| f(&pool_ref.0, pool_ref.1))
    }

    fn run_worker(self: Arc<Self>, index: usize, f: impl FnOnce(&Arc<Self>, usize)) {
        // Create a local pair of Pool + worker index
        // Then set it as the thread local.
        // Restore the old thread local value at the end to allow for nested calls.
        let pool_ref = Rc::new((self, index));
        let old_pool_ref = Self::tls_pool_ref().with(|rc| rc.replace(Some(pool_ref.clone())));

        pool_ref.0.emit(PoolEvent::WorkerSpawned {
            worker_index: index,
        });

        // Run the worker (Pool + worker index) pair
        // Then restore the thread local to its original value.
        f(&pool_ref.0, pool_ref.1);
        Self::tls_pool_ref().with(|rc| rc.replace(old_pool_ref));

        pool_ref.0.emit(PoolEvent::WorkerShutdown {
            worker_index: index,
        });
    }

    fn run(self: &Arc<Self>, index: usize) {
        let mut tick: u8 = 0;
        let mut is_waking = false;
        let mut xorshift = 0xdeadbeef + index;

        while let Some(waking) = self.wait(index, is_waking) {
            is_waking = waking;

            while let Some((task, stole)) = {
                let be_fair = tick % 64 == 0;
                self.pop(index, be_fair, &mut xorshift)
            } {
                if is_waking || stole {
                    self.notify(is_waking);
                    is_waking = false;
                }

                tick = tick.wrapping_add(1);
                unsafe {
                    let vtable = task.as_ref().vtable;
                    (vtable.poll_fn)(task, self, index)
                }
            }
        }
    }

    #[cold]
    fn pop(
        self: &Arc<Self>,
        index: usize,
        be_fair: bool,
        xorshift: &mut usize,
    ) -> Option<(NonNull<Task>, bool)> {
        if be_fair {
            if let Some(task) = self.pop_consume(index, index) {
                return Some((task, true));
            }
        }

        if let Some(task) = self.pop_local(index) {
            return Some((task, false));
        }

        if let Some(task) = self.pop_consume(index, index) {
            return Some((task, true));
        }

        self.pop_shared(index, xorshift).map(|task| (task, true))
    }

    fn pop_local(self: &Arc<Self>, index: usize) -> Option<NonNull<Task>> {
        // TODO: add worker-local injector consume here

        self.workers[index].buffer.pop().map(|task| {
            self.emit(PoolEvent::WorkerPopped {
                worker_index: index,
                task,
            });

            task
        })
    }

    #[cold]
    fn pop_shared(self: &Arc<Self>, index: usize, xorshift: &mut usize) -> Option<NonNull<Task>> {
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

        let num_workers = self.workers.len();
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
    fn pop_consume(self: &Arc<Self>, index: usize, target_index: usize) -> Option<NonNull<Task>> {
        self.workers[index]
            .buffer
            .consume(unsafe { Pin::new_unchecked(&self.workers[target_index].injector) })
            .map(|task| {
                self.emit(PoolEvent::WorkerStole {
                    worker_index: index,
                    target_index,
                });

                self.emit(PoolEvent::WorkerPopped {
                    worker_index: index,
                    task: task,
                });

                task
            })
    }

    #[cold]
    fn pop_steal(self: &Arc<Self>, index: usize, target_index: usize) -> Option<NonNull<Task>> {
        assert_ne!(index, target_index);
        self.workers[index]
            .buffer
            .steal(&self.workers[target_index].buffer)
            .map(|task| {
                self.emit(PoolEvent::WorkerStole {
                    worker_index: index,
                    target_index,
                });

                self.emit(PoolEvent::WorkerPopped {
                    worker_index: index,
                    task,
                });

                task
            })
    }
}
