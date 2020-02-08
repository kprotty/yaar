
pub trait Clock {
    type Instant: Default + PartialOrd<Self::Instant>;

    fn now() -> Self::Instant;
}
