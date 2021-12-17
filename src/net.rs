use super::waker::AtomicWaker;
use mio::{
    event::{Events, Source},
    Interest, Poll as Poller, Registry, Token,
};
use once_cell::sync::OnceCell;
use std::{
    fmt,
    future::Future,
    io::{self, Read, Write},
    mem::drop,
    net::{Shutdown, SocketAddr},
    pin::Pin,
    sync::atomic::{AtomicBool, Ordering},
    sync::{Arc, Mutex},
    task::{Context, Poll, Waker},
    thread,
};

#[derive(Default)]
struct IoAtomicWaker {
    waker: AtomicWaker,
    notified: AtomicBool,
}

impl IoAtomicWaker {
    fn poll(&self, ctx: &mut Context<'_>) -> Poll<()> {
        if self.notified.load(Ordering::Relaxed) {
            return Poll::Ready(());
        }

        let poll_result = self.waker.poll(ctx);
        if let Poll::Ready(_) = poll_result {
            self.notified.store(true, Ordering::Relaxed);
            self.waker.reset();
        }

        poll_result
    }

    fn reset(&self) {
        self.notified.store(false, Ordering::Relaxed);
    }

    fn wake(&self) -> Option<Waker> {
        self.waker.wake()
    }
}

#[derive(Copy, Clone, Eq, PartialEq, Debug)]
enum IoKind {
    Read = 0,
    Write = 1,
}

struct IoWaker {
    slot: usize,
    wakers: [IoAtomicWaker; 2],
}

#[derive(Default)]
struct IoStorage {
    idle: Vec<usize>,
    wakers: Vec<Option<Arc<IoWaker>>>,
}

impl IoStorage {
    fn alloc(&mut self) -> Arc<IoWaker> {
        if let Some(idle_index) = self.idle.len().checked_sub(1) {
            let index = self.idle.swap_remove(idle_index);
            return self.wakers[index].as_ref().unwrap().clone();
        }

        let waker = Arc::new(IoWaker {
            slot: self.wakers.len(),
            wakers: [IoAtomicWaker::default(), IoAtomicWaker::default()],
        });

        self.wakers.push(Some(waker.clone()));
        waker
    }

    fn free(&mut self, waker: &Arc<IoWaker>) {
        self.idle.push(waker.slot);
    }
}

struct Driver {
    registry: Registry,
    storage: Mutex<IoStorage>,
}

impl Driver {
    fn global() -> &'static io::Result<Self> {
        static GLOBAL_IO_DRIVER: OnceCell<io::Result<Driver>> = OnceCell::new();
        GLOBAL_IO_DRIVER.get_or_init(|| {
            let poller = Poller::new()?;
            let registry = poller.registry().try_clone()?;
            thread::spawn(move || Self::global().as_ref().unwrap().event_loop(poller));

            Ok(Self {
                registry,
                storage: Mutex::new(IoStorage::default()),
            })
        })
    }

    fn event_loop(&self, mut poller: Poller) {
        let mut wakers = Vec::new();
        let mut events = Events::with_capacity(1024);

        loop {
            let _ = poller.poll(&mut events, None);
            if events.is_empty() {
                continue;
            }

            let storage = self.storage.lock().unwrap();
            for event in events.iter() {
                let index = event.token().0;
                if let Some(waker) = storage.wakers[index].as_ref() {
                    if event.is_readable() {
                        if let Some(waker) = waker.wakers[IoKind::Read as usize].wake() {
                            wakers.push(waker);
                        }
                    }
                    if event.is_writable() {
                        if let Some(waker) = waker.wakers[IoKind::Write as usize].wake() {
                            wakers.push(waker);
                        }
                    }
                }
            }

            drop(storage);
            for waker in wakers.drain(..) {
                waker.wake();
            }
        }
    }
}

struct Pollable<S: Source> {
    source: S,
    waker: Arc<IoWaker>,
}

impl<S: Source> Pollable<S> {
    fn new(mut source: S) -> io::Result<Self> {
        let driver = match Driver::global().as_ref() {
            Ok(driver) => driver,
            Err(err) => return Err(io::Error::from(err.kind())),
        };

        let waker = driver.storage.lock().unwrap().alloc();
        if let Err(err) = driver.registry.register(
            &mut source,
            Token(waker.slot),
            Interest::READABLE | Interest::WRITABLE,
        ) {
            driver.storage.lock().unwrap().free(&waker);
            return Err(err);
        }

        Ok(Self { source, waker })
    }

    async fn poll_io<T>(
        &self,
        kind: IoKind,
        do_io: impl FnMut() -> io::Result<T> + Unpin,
    ) -> io::Result<T> {
        struct PollFuture<'a, S: Source, T, F: FnMut() -> io::Result<T> + Unpin> {
            pollable: &'a Pollable<S>,
            kind: IoKind,
            do_io: F,
        }

        impl<'a, S: Source, T, F: FnMut() -> io::Result<T> + Unpin> Future for PollFuture<'a, S, T, F> {
            type Output = io::Result<T>;

            fn poll(mut self: Pin<&mut Self>, ctx: &mut Context<'_>) -> Poll<Self::Output> {
                let kind = self.kind;
                let pollable = self.pollable;

                loop {
                    let waker = &pollable.waker.wakers[kind as usize];
                    if let Poll::Pending = waker.poll(ctx) {
                        return Poll::Pending;
                    }

                    loop {
                        match (self.do_io)() {
                            Err(e) if e.kind() == io::ErrorKind::WouldBlock => break waker.reset(),
                            Err(e) if e.kind() == io::ErrorKind::Interrupted => {}
                            result => return Poll::Ready(result),
                        }
                    }
                }
            }
        }

        PollFuture {
            pollable: self,
            kind,
            do_io,
        }
        .await
    }
}

impl<S: Source> Drop for Pollable<S> {
    fn drop(&mut self) {
        for kind in [IoKind::Read, IoKind::Write] {
            let waker = &self.waker.wakers[kind as usize];
            drop(waker.wake());
            waker.reset();
        }

        let driver = Driver::global().as_ref().unwrap();
        let _ = driver.registry.deregister(&mut self.source);
        driver.storage.lock().unwrap().free(&self.waker);
    }
}

pub struct TcpStream {
    pollable: Pollable<mio::net::TcpStream>,
}

impl fmt::Debug for TcpStream {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("TcpStream").finish()
    }
}

impl TcpStream {
    pub async fn connect(addr: SocketAddr) -> io::Result<Self> {
        let stream = mio::net::TcpStream::connect(addr)?;
        let stream = Self {
            pollable: Pollable::new(stream)?,
        };

        stream.pollable.poll_io(IoKind::Write, || Ok(())).await?;

        match stream.pollable.source.take_error()? {
            Some(err) => Err(err),
            None => Ok(stream),
        }
    }

    pub async fn read(&self, buffer: &mut [u8]) -> io::Result<usize> {
        self.pollable
            .poll_io(IoKind::Read, || (&self.pollable.source).read(buffer))
            .await
    }

    pub async fn write(&self, buffer: &[u8]) -> io::Result<usize> {
        self.pollable
            .poll_io(IoKind::Write, || (&self.pollable.source).write(buffer))
            .await
    }

    pub async fn read_vectored(&self, buffers: &mut [io::IoSliceMut<'_>]) -> io::Result<usize> {
        self.pollable
            .poll_io(IoKind::Read, || {
                (&self.pollable.source).read_vectored(buffers)
            })
            .await
    }

    pub async fn write_vectored(&self, buffers: &[io::IoSlice<'_>]) -> io::Result<usize> {
        self.pollable
            .poll_io(IoKind::Write, || {
                (&self.pollable.source).write_vectored(buffers)
            })
            .await
    }

    pub async fn peek(&self, buffer: &mut [u8]) -> io::Result<usize> {
        self.pollable
            .poll_io(IoKind::Read, || (&self.pollable.source).peek(buffer))
            .await
    }

    pub fn flush(&self) -> io::Result<()> {
        (&self.pollable.source).flush()
    }

    pub fn shutdown(&self, shutdown: Shutdown) -> io::Result<()> {
        (&self.pollable.source).shutdown(shutdown)
    }

    pub fn nodelay(&self) -> io::Result<bool> {
        (&self.pollable.source).nodelay()
    }

    pub fn set_nodelay(&self, nodelay: bool) -> io::Result<()> {
        (&self.pollable.source).set_nodelay(nodelay)
    }

    pub fn ttl(&self) -> io::Result<u32> {
        (&self.pollable.source).ttl()
    }

    pub fn set_ttl(&self, ttl: u32) -> io::Result<()> {
        (&self.pollable.source).set_ttl(ttl)
    }

    pub fn local_addr(&self) -> io::Result<SocketAddr> {
        (&self.pollable.source).local_addr()
    }

    pub fn peer_addr(&self) -> io::Result<SocketAddr> {
        (&self.pollable.source).peer_addr()
    }
}

pub struct TcpListener {
    pollable: Pollable<mio::net::TcpListener>,
}

impl fmt::Debug for TcpListener {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("TcpListener").finish()
    }
}

impl TcpListener {
    pub fn bind(addr: SocketAddr) -> io::Result<Self> {
        let listener = mio::net::TcpListener::bind(addr)?;
        let pollable = Pollable::new(listener)?;
        Ok(Self { pollable })
    }

    pub async fn accept(&self) -> io::Result<(TcpStream, SocketAddr)> {
        self.pollable
            .poll_io(IoKind::Read, || {
                let (stream, addr) = (&self.pollable.source).accept()?;
                let pollable = Pollable::new(stream)?;
                Ok((TcpStream { pollable }, addr))
            })
            .await
    }
}
