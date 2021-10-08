use super::{
    pool::Pool,
    queue::{Buffer, Injector, List},
};
use std::{cell::RefCell, pin::Pin, rc::Rc, sync::Arc};

#[derive(Default)]
pub(super) struct Worker {
    buffer: Buffer,
    injector: Injector,
}

pub(super) enum WorkerTask<'a> {
    Spawned(Pin<&'a Task>),
    Yielded(Pin<&'a Task>),
    Scheduled(Pin<&'a Task>),
    Injected(Pin<&'a Task>),
}

pub(crate) struct WorkerRef {
    pub(super) pool: Arc<Pool>,
    pub(super) index: usize,
}

impl WorkerRef {
    fn with_tls<T>(f: impl FnOnce(&mut Option<Rc<WorkerRef>>) -> T) -> T {
        thread_local!(static TLS: RefCell<Option<Rc<WorkerRef>>> = RefCell::new(None));
        TLS.with(|ref_cell| f(&mut *ref_cell.borrow_mut()))
    }

    pub(crate) fn with_current<T>(f: impl FnOnce(&WorkerRef) -> T) -> Option<T> {
        Self::with_tls(|tls| tls.as_ref().map(|rc| rc.clone())).map(|rc| f(&*rc))
    }

    pub(super) fn run(self) {
        let old_rc = Self::with_tls(|tls| mem::replace(tls, Some(Rc::new(self))));
        rc.run_worker();
        Self::with_tls(|tls| *tls = old_rc);
    }

    fn run_worker(&self) {}

    pub(super) unsafe fn schedule<'a>(&self, worker_task: WorkerTask<'a>) {
        Self::push(&self.pool, self.index, worker_task)
    }

    pub(super) unsafe fn push<'a>(
        pool: &Arc<Pool>,
        worker_index: usize,
        worker_task: WorkerTask<'a>,
    ) {
        let (task, be_fair) = match &worker_task {
            WorkerTask::Yielded(task) => (task, true),
            WorkerTask::Injected(task) => (task, true),
            WorkerTask::Spawned(task) => (task, false),
            WorkerTask::Scheduled(task) => (task, false),
        };

        let injector = Pin::new_unchecked(&pool.workers[worker_index].injector);
        let node = Pin::map_unchecked(task, |task| &task.node);

        if be_fair {
            injector.push(List::from(node));
        } else {
            pool.workers[worker_index].buffer.push(node, injector);
        }

        pool.notify()
    }
}
