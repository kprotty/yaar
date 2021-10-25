use super::{
    executor::{Executor, Worker},
    task::Task,
};
use std::{cell::RefCell, rc::Rc, sync::Arc};

pub struct Thread {
    executor: Arc<Executor>,
    worker_index: Option<usize>,
    tick: usize,
    xorshift: usize,
    is_searching: bool,
}

impl Thread {
    fn with_tls<F>(f: impl FnOnce(&mut Option<Rc<Thread>>) -> F) -> F {
        thread_local!(static ref TLS: RefCell<Option<Rc<Thread>>> = RefCell::new());
        TLS.with(|ref_cell| f(&mut *ref_cell.borrow_mut()))
    }

    pub fn with_current<F>(f: impl FnOnce(&Thread) -> F) -> Option<F> {
        Self::with_tls(|tls| tls.map(Rc::clone)).map(|rc| f(&*rc))
    }

    pub fn run(executor: Arc<Executor>, worker_index: usize, is_searching: bool) {
        let thread = Rc::new(Self {
            executor,
            worker_index: Some(worker_index),
            tick: 0,
            xorshift: worker_index + 0xdeadbeef,
            is_searching,
        });

        let old_tls = Self::with_tls(|tls| mem::replace(tls, Some(thread.clone())));
        thread.execute();
        Self::with_tls(|tls| *tls = old_tls);
    }

    fn execute(&self) {}
}
