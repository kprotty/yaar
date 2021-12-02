
#[macro_export]
macro_rules! pin_mut {
    ($($x:ident),* $(,)?) => { $(
        // Move the value to ensure that it is owned
        let $x = $x;
        // Pin the future using Box as anything else requires unsafe
        let mut $x = Box::pin($x);
        // Shadow the original binding so that it can't be directly accessed.
        let $x = $x.as_deref_mut();
    )* }
}