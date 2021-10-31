

pub struct Handle {

}

impl Clone for Handle {
    fn clone(&self) -> Self {

    }
}

impl Handle {
    pub fn current() -> Self {

    }

    pub fn try_current() -> Result<Self, ()> {
        
    }

    pub fn block_on<F: Future>(&self, future: F) -> F::Output {

    }

    pub fn spawn<F>(&self, future: F) -> JoinHandle<F::Output>
    where
        F: Future + Send + 'static,
        F::Output: Send + 'static
    {

    }

    pub fn enter(&self) -> EnterGuard<'_> {

    }
}