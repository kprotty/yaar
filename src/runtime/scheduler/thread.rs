use super::{executor::Executor, queue::Producer, worker::WorkerThread};
use parking_lot::{Condvar, Mutex};
use std::{
    cell::RefCell, collections::VecDeque, mem::replace, num::NonZeroUsize, rc::Rc, sync::Arc,
    thread,
};

struct Pool {
    idle: usize,
    spawned: usize,
    shutdown: bool,
    notified: VecDeque<usize>,
}

pub struct ThreadPool {
    max_threads: NonZeroUsize,
    condvar: Condvar,
    pool: Mutex<Pool>,
}

impl ThreadPool {
    pub fn new(max_threads: NonZeroUsize) -> Self {
        Self {
            max_threads,
            condvar: Condvar::new(),
            pool: Mutex::new(Pool {
                idle: 0,
                spawned: 0,
                shutdown: false,
                notified: VecDeque::with_capacity(max_threads.get()),
            }),
        }
    }

    pub fn spawn(&self, executor: &Arc<Executor>, worker_index: usize) -> Result<(), ()> {
        let mut pool = self.pool.lock();
        if pool.shutdown {
            return Err(());
        }

        if pool.notified.len() < pool.idle {
            pool.notified.push_back(worker_index);
            self.condvar.notify_one();
            return Ok(());
        }

        assert!(pool.spawned <= self.max_threads.get());
        if pool.spawned == self.max_threads.get() {
            return Err(());
        }

        let position = pool.spawned;
        pool.spawned += 1;
        drop(pool);

        if position == 0 {
            Self::run(executor, worker_index, position);
            return Ok(());
        }

        let executor = executor.clone();
        thread::Builder::new()
            .spawn(move || Self::run(&executor, worker_index, position))
            .map(drop)
            .map_err(|_| self.finish())
    }

    fn run(executor: &Arc<Executor>, worker_index: usize, position: usize) {
        Thread::run(executor, worker_index, position);
        executor.thread_pool.finish();
    }

    fn finish(&self) {
        let mut pool = self.pool.lock();
        assert!(pool.spawned <= self.max_threads.get());
        assert_ne!(pool.spawned, 0);
        pool.spawned -= 1;
    }

    pub fn wait(&self) -> Result<usize, ()> {
        let mut pool = self.pool.lock();
        loop {
            if pool.shutdown {
                return Err(());
            }

            if let Some(worker_index) = pool.notified.pop_back() {
                return Ok(worker_index);
            }

            pool.idle += 1;
            assert!(pool.idle <= self.max_threads.get());

            self.condvar.wait(&mut pool);

            assert!(pool.idle <= self.max_threads.get());
            assert_ne!(pool.idle, 0);
            pool.idle -= 1;
        }
    }

    pub fn shutdown(&self) {
        let mut pool = self.pool.lock();
        if replace(&mut pool.shutdown, true) {
            return;
        }

        if pool.idle > 0 && pool.notified.len() < pool.idle {
            self.condvar.notify_all();
        }
    }
}

pub struct Thread {
    pub executor: Arc<Executor>,
    pub producer: RefCell<Option<Producer>>,
}

impl Thread {
    fn with_tls<F>(f: impl FnOnce(&mut Option<Rc<Self>>) -> F) -> F {
        thread_local!(static TLS: RefCell<Option<Rc<Thread>>> = RefCell::new(None));
        TLS.with(|ref_cell| f(&mut *ref_cell.borrow_mut()))
    }

    pub fn try_with<F>(f: impl FnOnce(&Self) -> F) -> Option<F> {
        Self::with_tls(|tls| tls.as_ref().map(Rc::clone)).map(|rc| f(&*rc))
    }

    fn run(executor: &Arc<Executor>, worker_index: usize, position: usize) {
        let thread = Rc::new(Self {
            executor: executor.clone(),
            producer: RefCell::new(None),
        });

        let old_tls = Self::with_tls(|tls| replace(tls, Some(thread.clone())));
        WorkerThread::run(&*thread, worker_index, position);
        Self::with_tls(|tls| *tls = old_tls);
    }
}
