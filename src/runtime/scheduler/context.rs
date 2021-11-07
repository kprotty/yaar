use super::{executor::Executor, queue::{Runnable, Producer}};
use std::{cell::RefCell, collections::VecDeque, mem::replace, rc::Rc, sync::Arc};

pub struct Context {
    pub executor: Arc<Executor>,
    pub producer: RefCell<Option<Producer>>,
    pub intercept: RefCell<Option<VecDeque<Runnable>>>,
}

impl Context {
    fn with_tls<F>(f: impl FnOnce(&mut Option<ContextRef>) -> F) -> F {
        thread_local!(static TLS: RefCell<Option<ContextRef>> = RefCell::new(None));
        TLS.with(|ref_cell| f(&mut *ref_cell.borrow_mut()))
    }

    pub fn current() -> ContextRef {
        Self::try_current().expect("Called runtime-specific function outside of a runtime context")
    }

    pub fn try_current() -> Option<ContextRef> {
        Self::with_tls(|tls| tls.as_ref().map(|ctx_ref| ctx_ref.clone()))
    }

    pub fn enter(executor: &Arc<Executor>) -> ContextRef {
        Self::with_tls(|tls| {
            if let Some(ctx_ref) = tls.as_ref() {
                return ctx_ref.clone();
            }

            let ctx_ref = ContextRef {
                owned: true,
                context: Rc::new(Context {
                    executor: executor.clone(),
                    producer: RefCell::new(None),
                    intercept: RefCell::new(None),
                }),
            };

            *tls = Some(ctx_ref.clone());
            ctx_ref
        })
    }
}

pub struct ContextRef {
    owned: bool,
    context: Rc<Context>,
}

impl Clone for ContextRef {
    fn clone(&self) -> Self {
        Self {
            owned: false,
            context: self.context.clone(),
        }
    }
}

impl AsRef<Context> for ContextRef {
    fn as_ref(&self) -> &Context {
        &*self.context
    }
}

impl Drop for ContextRef {
    fn drop(&mut self) {
        if self.owned {
            self.drop_slow();
        }
    }
}

impl ContextRef {
    #[cold]
    fn drop_slow(&self) {
        assert!(self.owned);

        match Context::with_tls(|tls| replace(tls, None)) {
            Some(old_ref) => assert!(Rc::ptr_eq(&old_ref.context, &self.context)),
            None => unreachable!("enter()'d ContextRef dropping without thread local"),
        }
    }
}
