use super::task::TaskRunnable;
use std::sync::Arc;
use crate::dependencies::crossbeam_deque;

pub type Runnable = Arc<dyn TaskRunnable>;

pub type Steal = crossbeam_deque::Steal<Runnable>;
pub type Worker = crossbeam_deque::Worker<Runnable>;
pub type Stealer = crossbeam_deque::Stealer<Runnable>;
pub type Injector = crossbeam_deque::Injector<Runnable>;
