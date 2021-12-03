use std::task::{Context as PollContext, Poll};

#[derive(Clone)]
pub struct CoopBudget {
    quota: u8,
}

impl Default for CoopBudget {
    fn default() -> Self {
        Self { quota: 128 }
    }
}

impl CoopBudget {
    pub fn poll(&mut self, ctx: &mut PollContext<'_>) -> Poll<()> {
        if let Some(new_quota) = self.quota.checked_sub(1) {
            self.quota = new_quota;
            return Poll::Ready(());
        }

        *self = Self::default();
        ctx.waker().wake_by_ref();
        Poll::Pending
    }
}
