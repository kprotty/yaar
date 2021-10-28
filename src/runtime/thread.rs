use super::executor::{Executor, ExecutorRef};
use std::{cell::RefCell, rc::Rc, sync::Arc};

struct Inner {
    worker_index: usize,
    searching: bool,
}

pub struct Thread {
    executor: Arc<Executor>,
    inner: RefCell<Inner>,
}

impl Thread {
    fn with_tls<F>(f: impl FnOnce(&mut Option<Rc<Thread>>) -> F) -> F {
        thread_local!(static TLS: RefCell<Option<Rc<Thread>>> = RefCell::new(None));
        TLS.with(|ref_cell| f(&mut *ref_cell.borrow_mut()))
    }

    fn with_current<F>(f: impl FnOnce(&Thread) -> F) -> Option<F> {
        Self::with_tls(|tls| tls.as_ref().map(Rc::clone)).map(|rc| f(&*rc))
    }

    pub fn with_worker<F>(f: impl FnOnce(&Arc<ExecutorRef>, usize) -> F) -> Option<F> {
        unimplemented!("TODO")
    }

    fn run(executor: Arc<Executor>, worker_index: usize, searching: bool) {
        unimplemented!("TODO")
    }
}
