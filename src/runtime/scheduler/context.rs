use super::{executor::Executor, queue::Producer, random::Rng};
use std::{
    cell::{Cell, RefCell},
    ops::Deref,
    rc::Rc,
    sync::Arc,
};

pub struct Inner {
    pub rng: RefCell<Rng>,
    pub executor: Arc<Executor>,
    pub worker_index: Cell<Option<usize>>,
    pub producer: RefCell<Option<Producer>>,
}

impl Inner {
    fn with_tls<F>(f: impl FnOnce(&mut Option<Rc<Self>>) -> F) -> F {
        thread_local!(static TLS: RefCell<Option<Rc<Inner>>> = RefCell::new(None));
        TLS.with(|ref_cell| f(&mut *ref_cell.borrow_mut()))
    }
}

pub struct Context {
    inner: Rc<Inner>,
}

impl Context {
    pub fn current() -> Self {
        Self::try_current().expect("Called runtime-specific function outside of a runtime context")
    }

    pub fn try_current() -> Option<Rc<Self>> {
        Inner::with_tls(|tls| tls.as_ref().map(Rc::clone)).map(|inner| Self { inner })
    }

    pub fn enter(executor: &Arc<Executor>) -> Self {
        Self::with_tls(|tls| {
            if let Some(inner) = tls.as_ref().map(Rc::clone) {
                return Self { inner };
            }

            let inner = Rc::new(Inner {
                rng: RefCell::new(Rng::default()),
                executor: executor.clone(),
                worker_index: Cell::new(None),
                producer: RefCell::new(None),
            });

            let old_tls = replace(tls, Some(inner.clone()));
            assert!(old_tls.is_none(), "Nested block_on is not supported");

            Self { inner }
        })
    }
}

impl Deref for Context {
    type Target = Inner;

    fn deref(&self) -> &Inner {
        &*self.inner
    }
}

impl Clone for Context {
    fn clone(&self) -> Self {
        Self {
            inner: self.inner.clone(),
        }
    }
}

impl Drop for Context {
    fn drop(&mut self) {
        if Rc::strong_count(&self.inner) == 2 {
            let tls = Inner::with_tls(|tls| replace(tls, self.old_tls.replace(None)));
            let tls = tls.expect("Context reset TLS with invalid state");
            assert!(Rc::ptr_eq(tls, &self.inner));
        }
    }
}
