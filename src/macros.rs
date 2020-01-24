
macro_rules! cfg_os {
    ($($item:item)*) => {
        $(
            #[cfg(feature = "os")]
            #[cfg_attr(feature = "nightly", doc(cfg(feature = "os")))]
            $item
        )*
    }
}