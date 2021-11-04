use crate::internal::waker::AtomicWaker;
use arc_swap::ArcSwapOption;
use parking_lot::Mutex;
use std::{collections::VecDeque, convert::TryInto, mem::size_of, sync::Arc};

#[derive(Copy, Clone, Eq, PartialEq, Debug)]
pub enum WakerKind {
    Read = 0,
    Write = 1,
}

pub struct WakerIndex {
    block: u16,
    entry: u16,
}

impl Into<usize> for WakerIndex {
    fn into(self) -> usize {
        ((self.block as usize) << 16) | (self.entry as usize)
    }
}

impl From<usize> for WakerIndex {
    fn from(value: usize) -> Self {
        Self {
            block: (value & 0xffff).try_into().unwrap(),
            entry: ((value >> 16) & 0xffff).try_into().unwrap(),
        }
    }
}

const CHUNK_SIZE: usize = 64 * 1024;
const NUM_ENTRIES: usize = CHUNK_SIZE / size_of::<WakerEntry>();
const NUM_BLOCKS: usize = CHUNK_SIZE / size_of::<WakerBlockRef>();

type WakerEntry = [AtomicWaker; 2];
type WakerBlock = [WakerEntry; NUM_ENTRIES];
type WakerArray = [WakerBlockRef; NUM_BLOCKS];

type WakerBlockRef = ArcSwapOption<WakerBlock>;
type WakerArrayRef = ArcSwapOption<WakerArray>;

struct WakerAllocator {
    next_block: u16,
    available: VecDeque<WakerIndex>,
}

pub struct WakerStorage {
    array: WakerArrayRef,
    allocator: Mutex<WakerAllocator>,
}

impl WakerStorage {
    pub fn new() -> Self {
        Self {
            array: ArcSwapOption::const_empty(),
            allocator: Mutex::new(WakerAllocator {
                next_block: 0,
                available: VecDeque::new(),
            }),
        }
    }

    pub fn alloc(&self) -> Option<WakerIndex> {
        let mut allocator = self.allocator.lock();
        if let Some(index) = allocator.available.pop_back() {
            return Some(index);
        }

        let mut array = self.array.load();
        if array.as_ref().is_none() {
            drop(array);

            const EMPTY_BLOCK_REF: WakerBlockRef = ArcSwapOption::const_empty();
            const EMPTY_ARRAY: WakerArray = [EMPTY_BLOCK_REF; NUM_BLOCKS];
            self.array.store(Some(Arc::new(EMPTY_ARRAY)));
            array = self.array.load();
        }

        let array = array.as_ref().unwrap();
        if allocator.next_block as usize == NUM_BLOCKS {
            return None;
        }

        const EMPTY_WAKER: AtomicWaker = AtomicWaker::new();
        const EMPTY_ENTRY: WakerEntry = [EMPTY_WAKER; 2];
        const EMPTY_BLOCK: WakerBlock = [EMPTY_ENTRY; NUM_ENTRIES];
        array[allocator.next_block as usize].store(Some(Arc::new(EMPTY_BLOCK)));

        let next_block = allocator.next_block;
        allocator
            .available
            .extend((0..NUM_ENTRIES).rev().map(|entry| WakerIndex {
                block: next_block,
                entry: entry.try_into().unwrap(),
            }));

        allocator.next_block += 1;
        allocator.available.pop_back()
    }

    pub fn free(&self, index: WakerIndex) {
        let mut allocator = self.allocator.lock();
        allocator.available.push_back(index);
    }

    pub fn with<F>(&self, index: WakerIndex, f: impl FnOnce(&WakerEntry) -> F) -> F {
        let array = self.array.load();
        let array = array.as_ref().unwrap();

        let block = array[index.block as usize].load();
        let block = block.as_ref().unwrap();

        f(&block[index.entry as usize])
    }
}
