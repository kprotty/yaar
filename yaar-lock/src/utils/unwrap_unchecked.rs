pub trait UnwrapUnchecked<T> {
    unsafe fn unwrap_unchecked(self) -> T;
}

impl<T> UnwrapUnchecked<T> for Option<T> {
    unsafe fn unwrap_unchecked(self) -> T {
        self.unwrap_or_else(|| unreachable())
    }
}

impl<T, E> UnwrapUnchecked<T> for Result<T, E> {
    unsafe fn unwrap_unchecked(self) -> T {
        self.unwrap_or_else(|_| unreachable())
    }
}

pub unsafe fn unreachable() -> ! {
    if cfg!(debug_assertions) {
        unreachable!()
    } else {
        core::hint::unreachable_unchecked()
    }
}
