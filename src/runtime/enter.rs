use super::scheduler::{Executor, ThreadEnter};
use std::sync::Arc;

pub struct EnterGuard<'a> {
    _inner: ThreadEnter<'a>,
}

impl<'a> EnterGuard<'a> {
    pub(crate) fn new(executor: &'a Arc<Executor>) -> Self {
        Self {
            _inner: ThreadEnter::from(executor),
        }
    }
}
