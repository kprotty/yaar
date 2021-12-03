use super::{executor::Executor, queue::Producer};
use std::{
    cell::{Cell, RefCell},
    ops::Deref,
    rc::Rc,
};

pub struct InnerContext {
    pub executor: Arc<Executor>,
    pub producer: RefCell<Option<Producer>>,
    pub worker_index: Cell<Option<usize>>,
    pub coop_budget: RefCell<Option<CoopBudget>>,
}

impl InnerContext {
    fn with_tls<F>(f: impl FnOnce(&mut Option<Self>) -> F) -> F {
        thread_local!(static TLS: RefCell<Option<InnerContext>> = RefCell::new(None));
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
    pub fn enter(executor: &Arc<Executor>) -> Self {
        InnerContext::with_tls(|tls| {
            if let Some(inner) = tls.as_ref() {
                return Self {
                    inner: inner.clone(),
                };
            }

            let inner = Rc::new(InnerContext {
                executor: executor.clone(),
                producer: RefCell::new(None),
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

    pub fn current() -> Self {
        Self::try_current().expect("Using runtime functionality outside of the runtime context")
    }
}
