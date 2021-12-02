use super::queue::Worker as QueueWorker;
use std::{
    cell::{Cell, RefCell},
    ops::Deref,
    rc::Rc,
};

pub struct InnerContext {
    pub queue_worker: RefCell<Option<QueueWorker>>,
    pub worker_index: Cell<Option<usize>>,
}

impl InnerContext {
    fn with_tls<F>(f: impl FnOnce(&mut Option<Rc<Self>>) -> F) -> F {
        thread_local!(static TLS: RefCell<Option<Rc<InnerContext>>> = RefCell::new(None));
        TLS.with(|ref_cell| f(&mut *ref_cell.borrow_mut()))
    }
}

pub struct Context {
    inner: Rc<InnerContext>,
}

impl Deref for Context {
    type Target = InnerContext;

    fn deref(&self) -> &Self::Target {
        &*self.inner
    }
}

impl Drop for Context {
    fn drop(&mut self) {
        // 1 = Rc stored in tls
        // 1 = Rc stored in this Context
        if Rc::strong_count(&self.inner) == 2 {
            InnerContext::with_tls(|tls| *tls = None);
        }
    }
}

impl Context {
    pub fn enter() -> Self {
        InnerContext::with_tls(|tls| {
            if let Some(inner) = tls.as_ref() {
                return Self {
                    inner: inner.clone(),
                };
            }

            let inner = Rc::new(InnerContext {
                queue_worker: RefCell::new(None),
                worker_index: Cell::new(None),
            });

            *tls = Some(inner.clone());
            Self { inner }
        })
    }

    pub fn try_current() -> Option<Self> {
        InnerContext::with_tls(|tls| {
            tls.as_ref().map(|inner| Self {
                inner: inner.clone(),
            })
        })
    }
}
