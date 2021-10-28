use super::task::Task;
use std::{
    hint::spin_loop as spin_loop_hint,
    mem,
    pin::Pin,
    ptr::{self, NonNull},
    sync::atomic::{AtomicPtr, AtomicUsize, Ordering},
};

#[derive(Default)]
pub struct List {
    head: Option<NonNull<Task>>,
    tail: Option<NonNull<Task>>,
}

impl<'a> From<Pin<&'a Task>> for List {
    fn from(task: Pin<&'a Task>) -> Self {
        task.next.store(ptr::null_mut(), Ordering::Relaxed);
        List {
            head: Some(NonNull::from(&*task)),
            tail: Some(NonNull::from(&*task))
        }
    }
}

impl List {
    pub fn push(&mut self, list: impl Into<List>) {
        let list = list.into();
        list.head.map(|list_head| unsafe {
            match mem::replace(&mut self.tail, list.tail) {
                Some(tail) => tail.as_ref().next.store(list_head.as_ptr(), Ordering::Relaxed),
                None => self.head = Some(list_head),
            }
        })
    }
}

impl Iterator for List {
    type Item = NonNull<Task>;

    fn next(&mut self) -> Option<Self::Item> {
        let task = self.head?;
        self.head = NonNull::new(unsafe { task.as_ref().next.load(Ordering::Relaxed) });
        if self.head.is_none() {
            self.tail = None;
        }
        task
    }
}

#[derive(Copy, Clone, Eq, PartialEq, Debug)]
pub enum Error {
    Empty,
    Contended,
}

#[derive(Default)]
pub struct Queue {
    buffer: Buffer,
    injector: Injector,
}

impl Queue {
    fn injector(self: Pin<&Self>) -> Pin<&Injector> {
        unsafe { self.map_unchecked(|queue| &queue.injector) };
    }

    pub fn fill(self: Pin<&Self>, mut list: List) {
        self.buffer.fill(&mut list);
        self.injector().push(list);
    }

    pub fn inject(self: Pin<&Self>, list: List) {
        self.injector().push(list);
    }

    pub fn push(self: Pin<&Self>, task: Pin<&Task>) {
        self.buffer.push(task, self.as_ref().injector());
    }

    pub fn pop(self: Pin<&Self>, be_fair: bool) -> Option<NonNull<Task>> {
        let injector = self.as_ref().injector();
        if be_fair {
            self.buffer
                .consume(injector)
                .ok()
                .or_else(|| self.buffer.pop())
        } else {
            self.buffer
                .pop()
                .or_else(|| self.buffer.consume(injector).ok())
        }
    }

    pub fn steal(self: Pin<&Self>, queue: Pin<&Self>) -> Result<NonNull<Task>, Error> {
        self.buffer
            .consume(queue.as_ref().injector())
            .or_else(|err| self.buffer.steal(&queue.buffer).ok_or(err))
    }
}

#[derive(Default)]
struct Injector {
    head: AtomicPtr<Task>,
    tail: AtomicPtr<Task>,
    stub: Task,
}

impl Injector {
    pub fn push(self: Pin<&Self>, list: List) {
        list.head.map(|head| unsafe {
            let tail = list.tail.expect("List with head and not tail");
            debug_assert_eq!(tail.as_ref().next.load(Ordering::Relaxed), ptr::null_mut());

            let old_tail = self.tail.swap(tail.as_ptr(), Ordering::AcqRel);
            let prev = NonNull::new(old_tail).unwrap_or(NonNull::from(&self.stub));
            prev.as_ref().next.store(head.as_ptr(), Ordering::Release);
        })
    }

    pub fn consume(self: Pin<&Self>) -> Result<impl Iterator<Item = NonNull<Task>> + '_, Error> {
        struct Consumer<'a> {
            injector: Pin<&'a Self>,
            head: NonNull<Task>,
        }

        impl<'a> Iterator for Consumer<'a> {
            type Item = NonNull<Task>;

            fn next(&mut self) -> Option<Self::Item> {
                unsafe {
                    let stub = NonNull::from(&self.injector.stub);
                    if self.head == stub {
                        self.head = NonNull::new(self.head.as_ref().next.load(Ordering::Acquire))?;
                    }

                    let next = NonNull::new(self.head.as_ref().next.load(Ordering::Acquire)).or_else(|| {
                        let tail = self.injector.tail.load(Ordering::Relaxed);
                        if self.head != NonNull::new(tail) {
                            return None;
                        }
                        
                        let stub = Pin::new_unchecked(stub.as_ref());
                        self.injector.push(List::from(stub));
                        
                        spin_loop_hint();
                        NonNull::new(self.head.as_ref().next.load(Ordering::Acquire))
                    })?;

                    mem::replace(&mut self.head, next)
                }
            }
        }

        impl<'a> Drop for Consumer<'a> {
            fn drop(&mut self) {
                let stub = NonNull::from(&self.injector.stub).as_ptr();

                let mut new_head = self.head.as_ptr();
                if new_head == stub {
                    new_head = ptr::null_mut();
                }

                debug_assert_eq!(self.injector.head.load(Ordering::Relaxed), stub);
                self.injector.head.store(new_head, Ordering::Release);
            }
        }

        let stub = NonNull::from(&self.injector.stub);

        let tail = NonNull::new(self.tail.load(Ordering::Relaxed)).unwrap_or(stub);
        if tail == stub {
            return Err(Error::Empty);
        }

        let head = NonNull::new(self.head.swap(stub.as_ptr(), Ordering::Acquire));
        if head == Some(stub) {
            return Err(Error::Contended);
        }

        Ok(Consumer {
            injector: self,
            head: head.unwrap_or(stub),
        })
    }
}

struct Buffer {
    head: AtomicUsize,
    tail: AtomicUsize,
    array: [AtomicPtr<Task>; Self::CAPACITY],
}

impl Default for Buffer {
    fn default() -> Self {
        const EMPTY: AtomicPtr<Task> = AtomicPtr::new(ptr::null_mut());
        Self {
            head: AtomicUsize::new(0),
            tail: AtomicUsize::new(0),
            array: [EMPTY; Self::CAPACITY],
        }
    }
}

impl Buffer {
    const CAPACITY: usize = 256;

    fn write(&self, index: usize, task: NonNull<Task>) {
        let slot = &self.array[index % self.array.len()];
        slot.store(task.as_ptr(), Ordering::Relaxed);
    }

    fn read(&self, index: usize) -> NonNull<Task> {
        let slot = &self.array[index % self.array.len()];
        NonNull::new(slot.load(Ordering::Relaxed)).unwrap()
    }

    pub fn push(&self, task: Pin<&Task>, injector: Pin<&Injector>) {
        let task = NonNull::from(&*task);
        let tail = self.tail.load(Ordering::Relaxed);
        let mut head = self.head.load(Ordering::Relaxed);

        loop {
            let size = tail.wrapping_sub(head);
            assert!(size <= self.array.len());

            if size < self.array.len() {
                self.write(tail, task);
                self.tail.store(tail.wrapping_add(1), Ordering::Release);
                return;
            }

            let migrate = size / 2;
            if migrate == 0 {
                return injector.push(List::from(task));
            }

            if let Err(new_head) = self.head.compare_exchange_weak(
                head,
                head.wrapping_add(migrate),
                Ordering::Acquire,
                Ordering::Relaxed,
            ) {
                head = new_head;
                continue;
            }

            let mut overflowed = List::default();
            for offset in 0..migrate {
                let index = head.wrapping_add(offset);
                let migrated = self.read(index);
                overflowed.push(Pin::new_unchecked(migrated.as_ref()));
            }

            overflowed.push(task);
            injector.push(overflowed);
            return;
        }
    }

    pub fn pop(&self) -> Option<NonNull<Task>> {
        let tail = self.tail.load(Ordering::Relaxed);
        let mut head = self.head.load(Ordering::Relaxed);

        loop {
            let size = tail.wrapping_sub(head);
            assert!(size <= self.array.len());

            if size == 0 {
                return None;
            }

            match self.head.compare_exchange_weak(
                head,
                head.wrapping_add(1),
                Ordering::Acquire,
                Ordering::Relaxed,
            ) {
                Ok(_) => return Some(self.read(head)),
                Err(new_head) => head = new_head,
            }
        }
    }

    pub fn steal(&self, buffer: &Self) -> Option<NonNull<Task>> {
        if ptr::eq(self as *const Self, buffer as *const Self) {
            return None;
        }

        loop {
            let buffer_head = buffer.head.load(Ordering::Acquire);
            let buffer_tail = buffer.tail.load(Ordering::Acquire);

            let buffer_size = buffer_tail.wrapping_sub(buffer_head);
            if buffer_size == 0 {
                return None;
            }

            let buffer_steal = buffer_size - (buffer_size / 2);
            assert_ne!(buffer_steal, 0);
            if buffer_steal > buffer.array.len() / 2 {
                spin_loop_hint();
                continue;
            }

            let head = self.head.load(Ordering::Relaxed);
            let tail = self.tail.load(Ordering::Relaxed);
            assert_eq!(head, tail);

            for offset in 0..buffer_steal {
                let task = buffer.read(buffer_head.wrapping_add(offset));
                self.write(tail.wrapping_add(offset), task);
            }

            if let Err(_) = buffer.head.compare_exchange(
                buffer_head,
                buffer_head.wrapping_add(buffer_steal),
                Ordering::AcqRel,
                Ordering::Relaxed,
            ) {
                spin_loop_hint();
                continue;
            }

            let new_tail = tail.wrpaping_add(buffer_steal - 1);
            if tail != new_tail {
                self.tail.store(new_tail, Ordering::Release);
            }

            return Some(self.read(new_tail));
        }
    }

    pub fn consume(&self, injector: Pin<&Injector>) -> Result<NonNull<Task>, Error> {
        injector.consume().and_then(|mut consumer| {
            consumer.next().ok_or(Error::Empty).map(|consumed| {
                self.fill(&mut consumer);
                consumed
            })
        })
    }

    pub fn fill(&self, tasks: impl Iterator<Item = NonNull<Task>>) {
        let head = self.head.load(Ordering::Relaxed);
        let tail = self.tail.load(Ordering::Relaxed);

        let size = tail.wrapping_sub(head);
        assert!(size <= self.array.len());

        let available = self.array.len() - size;
        let new_tail = tasks.take(available).fold(tail, |new_tail, task| {
            self.write(new_tail, task);
            new_tail.wrapping_add(1)
        });

        if tail != new_tail {
            self.tail.store(new_tail, Ordering::Release);
        }
    }
}
