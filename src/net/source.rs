

#[derive(Copy, Clone, Eq, PartialEq)]
pub enum Kind {
    Read = 0,
    Write = 1,
}

struct Event {
    wakers: Arc<[AtomicWaker; 2]>,
    index: usize,
} 

struct EventCache {
    events: Vec<Arc<[AtomicWaker; 2]>>,
    idle: Vec<usize>,
}

impl EventCache {
    fn alloc(&mut self) -> Event {
        let index = if let Some(index) = self.idle.len().checked_sub(1) {
            self.idle.swap_remove(index)
        } else {
            self.events.push(Arc::new([AtomicWaker::default(), AtomicWaker::default()]));
            self.events.len() - 1
        };
        
        Event {
            wakers: self.events[index].clone(),
            index,
        }
    }

    fn free(&mut self, event: &Event) {
        self.idle.push(event.index);
    }
}

struct Poller {
    poll: mio::Poll,
    events: mio::event::Events,
}

struct Registry {
    poller: Mutex<Poller>,
    cache: Mutex<EventCache>,
    inner: mio::Registry,
}

impl Registry {
    fn get() -> &'static Self {
        static GLOBAL: OnceCell<Registry> = OnceCell::new();
        GLOBAL.get_or_init(|| {

        })
    }

    fn poll(&self) {
        let mut poller = self.poller.lock().unwrap();
    }
}

pub struct PollSource<S: Source> {
    source: S,
    event: Event,
}

impl<S: Source> PollSource<S> {
    pub fn new(mut source: S) -> io::Result<Self> {
        let registry = Registry::get();
        let event = registry.cache.lock().unwrap().alloc();

        if let Err(error) = registry
            .inner
            .register(
                &mut source,
                mio::Token(event.index),
                mio::Interest::READABLE | mio::Interest::WRITABLE,
            )
        {
            registry.cache.lock().unwrap().free(&event);
            return Err(error);
        }

        Ok(Self {
            source,
            event,
        })
    }
}

impl<S: Source> AsRef<S> PollSource<S> {
    fn as_ref(&self) -> &S {
        &self.source
    }
}

impl<S: Source> Drop PollSource<S> {
    fn drop(&mut self) {
        let registry = Registry::get();
        let _ = registry.inner.deregister(&mut self.source);

        for kind in [Kind::Read, Kind::Write] {
            self.event.wakers[kind as usize].wake();
        }

        registry.cache.lock().unwrap().free(&self.event);
    }
}