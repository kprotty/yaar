
pub trait Clock {
    type Tick;

    fn get_tick() -> Tick;
}