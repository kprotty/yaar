use super::task::Task;
use std::{
    mem,
    pin::Pin,
    ptr::{self, NonNull},
    hint::spin_loop as spin_loop_hint,
    sync::atomic::{AtomicPtr, AtomicUsize, Ordering},
};

#[derive(Default)]
pub struct List {
    head: Option<NonNull<Task>>,
    tail: Option<NonNull<Task>>,
}

impl List {
    pub unsafe fn push(&mut self, task: NonNull<Task>) {
        task.as_ref().next.store(ptr::null_mut(), Ordering::Relaxed);
        match mem::replace(&mut self.tail, Some(task)) {
            Some(tail) => tail.as_ref().next.store(task.as_ptr(), Ordering::Relaxed),
            None => self.head = Some(task),
        }
    }
}

impl Iterator for List {
    type Item = NonNull<Task>;

    fn next(&mut self) -> Option<Self::Item> {
        self.head.map(|task| unsafe {
            self.head = NonNull::new(task.as_ref().next.load(Ordering::Relaxed));
            if self.head.is_none() {
                self.tail = None;
            }
            task
        })
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

    pub unsafe fn fill(self: Pin<&Self>, mut list: List) {
        self.buffer.fill(&mut list);
        self.injector().push(list);
    }

    pub unsafe fn inject(self: Pin<&Self>, list: List) {
        self.injector().push(list);
    }

    pub unsafe fn push(self: Pin<&Self>, task: NonNull<Task>) {
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
    pub unsafe fn push(self: Pin<&Self>, list: List) {
        list.head.map(|head| {
            let tail = list.tail.expect("List with head and not tail");
            assert_eq!(tail.as_ref().next.load(Ordering::Relaxed), ptr::null_mut());

            let old_tail = self.tail.swap(tail.as_ptr(), Ordering::AcqRel);
            let prev = NonNull::new(old_tail).unwrap_or(NonNull::from(&self.stub));
            prev.as_ref().next.store(head.as_ptr(), Ordering::Release);
        })
    }

    pub fn consume(self: Pin<&Self>) -> Result<impl Iterator<Item = NonNull<Task>> + '_, Error> {
        struct Consumer<'a> {
            injector: Pin<&'a Self>,
            head: Option<NonNull<Task>>,
        }

        impl<'a> Iterator for Consumer<'a> {
            type Item = NonNull<Task>;

            fn next(&mut self) -> Option<Self::Item> {
                unsafe {
                    let stub = NonNull::from(&self.injector.stub);

                    let mut head = self.head.unwrap_or(stub);
                    if ptr::eq(head.as_ptr(), stub.as_ptr()) {
                        head = NonNull::new(head.as_ref().next.load(Ordering::Acquire))?;
                    }
                    
                    if let Some(new_head) = NonNull::new(head.as_ref().next.load(Ordering::Acquire)) {
                        self.head = Some(new_head);
                        return Some(head);
                    }

                    let tail = self.injector.tail.load(Ordering::Relaxed);
                    if ptr::eq(head.as_ptr(), tail) {
                        self.head = None;
                        stub.as_ref().next.store(ptr::null_mut(), Ordering::Relaxed);

                        match self.injector.tail.compare_exchange(
                            tail,
                            ptr::null_mut(),
                            Ordering::AcqRel,
                            Ordering::Acquire,
                        ) {
                            Ok(_) => return Some(head),
                            Err(_) => self.head = Some(head),
                        }
                    }
                    
                    spin_loop_hint();
                    self.head = NonNull::new(head.as_ref().next.load(Ordering::Acquire))?;
                    Some(head)
                }
            }
        }

        impl<'a> Drop for Consumer<'a> {
            fn drop(&mut self) {
                let new_head = self.head.map(|head| head.as_ptr()).unwrap_or(ptr::null_mut());
                let stub = NonNull::from(&self.injector.stub).as_ptr();
                assert_ne!(new_head, stub);

                assert_eq!(self.injector.head.load(Ordering::Relaxed), stub);
                self.injector.head.store(new_head, Ordering::Release);
            }
        }

        if self.tail.load(Ordering::Relaxed).is_null() {
            return Err(Error::Empty);
        }

        let stub = NonNull::from(&self.stub).as_ptr();
        if ptr::eq(self.head.load(Ordering::Relaxed), stub) {
            return Err(Error::Contended);
        }

        let head = self.head.swap(stub, Ordering::Acquire);
        if ptr::eq(head, stub) {
            return Err(Error::Contended);
        }

        Ok(Consumer {
            injector: self,
            head: NonNull::new(head),
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

    pub unsafe fn push(&self, task: NonNull<Task>, injector: Pin<&Injector>) {
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
                overflowed.push(self.read(index));
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
        let new_tail = tasks
            .take(available)
            .fold(tail, |new_tail, task| {
                self.write(new_tail, task);
                new_tail.wrapping_add(1)
            });

        if tail != new_tail {
            self.tail.store(new_tail, Ordering::Release);
        }
    }
}