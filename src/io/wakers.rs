use crate::sync::waker::AtomicWaker;
use arc_swap::ArcSwapOption;
use parking_lot::Mutex;
use std::{collections::VecDeque, convert::TryInto, sync::Arc};

#[derive(Copy, Clone, Eq, PartialEq, Debug)]
pub enum WakerKind {
    Read = 0,
    Write = 1,
}

const NUM_BLOCKS: usize = 256;
const NUM_ENTRIES: usize = 16 * 1024;

type WakerEntry = [AtomicWaker; 2];
type WakerBlock = [WakerEntry; NUM_ENTRIES];
type WakerArray = [ArcSwapOption<WakerBlock>; NUM_BLOCKS];

#[derive(Copy, Clone, Eq, PartialEq)]
pub struct WakerIndex {
    block: u8,
    entry: u16,
}

impl Into<usize> for WakerIndex {
    fn into(self) -> usize {
        (self.block as usize) | ((self.entry as usize) << 8)
    }
}

impl From<usize> for WakerIndex {
    fn from(value: usize) -> Self {
        Self {
            block: (value & 0xff).try_into().unwrap(),
            entry: ((value >> 8) & 0xffff).try_into().unwrap(),
        }
    }
}

struct WakerCache {
    indices: VecDeque<WakerIndex>,
    blocks: u8,
}

pub struct Wakers {
    array: ArcSwapOption<WakerArray>,
    cache: Mutex<WakerCache>,
}

impl Default for Wakers {
    fn default() -> Self {
        Self {
            array: ArcSwapOption::<WakerArray>::const_empty(),
            cache: parking_lot::const_mutex(WakerCache {
                indices: VecDeque::new(),
                blocks: 0,
            }),
        }
    }
}

impl Wakers {
    pub fn alloc(&self) -> Option<WakerIndex> {
        let mut cache = self.cache.lock();

        if let Some(index) = cache.indices.pop_front() {
            return Some(index);
        }

        let block = cache.blocks;
        cache.blocks = match cache.blocks.checked_add(1) {
            Some(next_block) => next_block,
            None => return None,
        };

        let mut array = self.array.load();
        if array.is_none() {
            const EMPTY_ARC_BLOCK: ArcSwapOption<WakerBlock> = ArcSwapOption::const_empty();
            const EMPTY_ARRAY: WakerArray = [EMPTY_ARC_BLOCK; NUM_BLOCKS];
            self.array.store(Some(Arc::new(EMPTY_ARRAY)));
            array = self.array.load();
        }

        const EMPTY_WAKER: AtomicWaker = AtomicWaker::new();
        const EMPTY_ENTRY: WakerEntry = [EMPTY_WAKER; 2];
        const EMPTY_BLOCK: WakerBlock = [EMPTY_ENTRY; NUM_ENTRIES];
        array.as_ref().unwrap()[block as usize].store(Some(Arc::new(EMPTY_BLOCK)));

        cache
            .indices
            .extend((0..NUM_ENTRIES).map(|index| WakerIndex {
                block,
                entry: index.try_into().unwrap(),
            }));

        Some(cache.indices.pop_front().unwrap())
    }

    pub fn free(&self, index: WakerIndex) {
        let mut cache = self.cache.lock();
        cache.indices.push_back(index);
    }

    pub(super) fn with<F>(&self, index: WakerIndex, f: impl FnOnce(&WakerEntry) -> F) -> F {
        let array = self.array.load();
        let array = array
            .as_ref()
            .expect("Accessing waker index without having allocated backing array");

        let block = array[index.block as usize].load();
        let block = block
            .as_ref()
            .expect("Accessing waker index without having allocated backing block");

        f(&block[index.entry as usize])
    }
}
