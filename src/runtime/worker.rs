use std::{cell::RefCell, rc::Rc, sync::Arc};

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
        let rc = Rc::new(self);
        let old_rc = Self::with_tls(|tls| mem::replace(tls, Some(rc.clone())));
        rc.run_worker();
        Self::with_tls(|tls| *tls = old_rc);
    }

    fn run_worker(&self) {}
}
