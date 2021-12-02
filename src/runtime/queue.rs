use super::task::TaskRunnable;
use crate::dependencies::crossbeam_deque;
use std::sync::Arc;

pub type Runnable = Arc<dyn TaskRunnable>;

pub type Steal = crossbeam_deque::Steal<Runnable>;
pub type Worker = crossbeam_deque::Worker<Runnable>;
pub type Stealer = crossbeam_deque::Stealer<Runnable>;
pub type Injector = crossbeam_deque::Injector<Runnable>;
