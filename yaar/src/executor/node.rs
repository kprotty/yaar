use super::{
    Platform,
};
use core::{
    cell::Cell,
};

pub struct Node<P: Platform> {
    pub(crate) scheduler: Cell<Option<NonNull<Scheduler<P>>>>,

}