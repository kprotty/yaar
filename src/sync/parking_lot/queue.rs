// Copyright (c) 2021 kprotty
//
// Licensed under the Apache License, Version 2.0 (the "License");
// you may not use this file except in compliance with the License.
// You may obtain a copy of the License at
//
// 	http://www.apache.org/licenses/LICENSE-2.0
//
// Unless required by applicable law or agreed to in writing, software
// distributed under the License is distributed on an "AS IS" BASIS,
// WITHOUT WARRANTIES OR CONDITIONS OF ANY KIND, either express or implied.
// See the License for the specific language governing permissions and
// limitations under the License

use super::lock::Lock as WaitLock;
use core::{
    pin::Pin,
    cell::Cell,
    ptr::NonNull,
    mem::size_of,
    hint::unreachable_unchecked,
    marker::PhantomPinned,
    cmp::Ordering as CmpOrdering,
    sync::atomic::{AtomicUsize, Ordering},
};

struct WaitNode {
    _pinned: PhantomPinned,
    address: Cell<usize>,
    ticket: Cell<usize>,
    parent: Cell<Option<NonNull<Self>>>,
    children: [Cell<Option<NonNull<Self>>>; 2],
    next: Cell<Option<NonNull<Self>>>,
    prev: Cell<Option<NonNull<Self>>>,
    tail: Cell<Option<NonNull<Self>>>,
}

struct WaitQueue<'a> {
    address: usize,
    tree: &'a WaitTree,
    head: Cell<Option<NonNull<WaitNode>>>,
    parent: Cell<Option<NonNull<WaitNode>>>,
}

impl<'a> WaitQueue<'a> {
    unsafe fn from_addr(tree: &'a WaitTree, address: usize) -> WaitQueue<'a> {
        let mut parent = None;
        let mut current = tree.root.get();

        while let Some(node) = current {
            let is_left = match address.cmp(node.as_ref().address.get()) {
                CmpOrdering::Less => true,
                CmpOrdering::Greater => false,
                CmpOrdering::Equal => return WaitQueue {
                    address,
                    tree,
                    head: Cell::new(Some(node)),
                    parent: Cell::new(parent),
                },
            };

            parent = Some(node);
            current = node.as_ref().children[!is_left as usize].get();
        }

        WaitQueue {
            address,
            tree,
            head: Cell::new(None),
            parent: Cell::new(parent),
        }
    }

    unsafe fn from_node(tree: &'a WaitTree, node: &WaitNode) -> WaitQueue<'a> {
        let address = node.address.get();

        if node.as_ref().prev.get().is_none() {
            return WaitQueue {
                address,
                tree,
                head: Cell::new(Some(NonNull::from(node))),
                parent: Cell::new(node.parent.get()),
            };
        }

        Self::from_addr(tree, address)
    }

    unsafe fn insert(&self, node: Pin<&WaitNode>) {
        node.address.set(self.address);
        node.prev.set(None);
        node.next.set(None);
        node.tail.set(Some(NonNull::from(&*node)));

        if let Some(head) = self.head.get() {
            let tail = head.as_ref().tail.get();
            let tail = tail.unwrap_or_else(|| unreachable_unchecked());
            tail.as_ref().next.set(Some(NonNull::from(&*node)));
            node.prev.set(Some(tail));
            head.as_ref().tail.set(Some(NonNull::from(&*node)));
            return;
        }

        self.tree.insert(node);
        self.head.set(Some(NonNull::from(&*node)));
    }

    unsafe fn remove(&self, node: Pin<&WaitNode>) {

    }
}

struct WaitTree {
    root: Cell<Option<NonNull<WaitNode>>>,
    prng: Cell<usize>,
}

impl WaitTree {
    unsafe fn insert(&self, parent: Option<NonNull<WaitNode>>, node: NonNull<Node>) {
        node.as_ref().parent.set(parent);
        node.as_ref().children[0].set(None);
        node.as_ref().children[1].set(None);

        if let Some(parent) = parent {
            let is_left = self.address < parent.as_ref().address;
            parent.as_ref().children[!is_left as usize].set(Some(node));
        } else {
            self.root.set(Some(node));
        }
        
        let ticket = self.tree.gen_rng() | 1;
        node.as_ref().ticket.set(ticket);
        while let Some(parent) = node.as_ref().parent.get() {
            if parent.as_ref().ticket.get() <= ticket {
                break;
            }

            let is_left = parent.as_ref().children[0].get() == Some(node);
            self.rotate(parent, is_left);
        }
    }

    unsafe fn remove(&self, node: NonNull<Node>) {
        while node.as_ref().children.iter().any(|s| s.get().is_some()) {
            compile_error!("TODO: https://golang.org/src/runtime/sema.go");
        }
    }

    unsafe fn replace(&self, node: NonNull<WaitNode>, new_node: Option<NonNull<WaitNode>>) {
        if let Some(new_node) = new_node {
            new_node.as_ref().ticket.set(node.as_ref().ticket.get());
            new_node.as_ref().parent.set(node.as_ref().parent.get());
            new_node.as_ref().children[0].set(node.as_ref().children[0].get());
            new_node.as_ref().children[1].set(node.as_ref().children[1].get());
        }

        for child in node.children.iter().filter_map(|c| c.get()) {
            child.as_ref().parent.set(new_node);
        }

        if let Some(parent) = node.as_ref().parent.get() {
            let is_left = parent.as_ref().children[0].get() == Some(node);
            parent.as_ref().children[!is_left as usize].set(new_node);
        } else {
            self.root.set(new_node);
        }
    }

    unsafe fn rotate(&self, node: NonNull<WaitNode>, is_left: bool) {
        let parent = node.as_ref().parent.get();
        let target = node.as_ref().children[is_left as usize].get();
        let target = target.unwrap_or_else(|| unreachable_unchecked());
        let child = target.as_ref().children[!is_left as usize].get();

        target.as_ref().children[!is_left as usize].set(Some(node));
        node.as_ref().parent.set(Some(node));
        node.as_ref().children[is_left as usize].set(child);
        if let Some(child) = child {
            child.as_ref().parent.set(Some(node));
        }

        target.as_ref().parent.set(parent);
        if let Some(parent) = parent {
            let is_left = parent.as_ref().children[0].get() == Some(node);
            parent.as_ref().children[!is_left as usize].set(Some(target));
        } else {
            self.root.set(Some(target));
        }
    }

    fn gen_rng(&self) -> usize {
        let (a, b, c) = match size_of::<usize>() {
            8 => (13, 7, 17),
            4 => (13, 17, 5),
            2 => (7, 9, 8),
            _ => unreachable!("architecture not supported"),
        };
        
        let xorshift = self.prng.get();
        xorshift ^= xorshift << a;
        xorshift ^= xorshift >> b;
        xorshift ^= xorshift << c;
        self.prng.set(xorshift);

        xorshift
    }
}

struct WaitBucket {
    waiters: AtomicUsize,
    lock: WaitLock<WaitTree>,
}