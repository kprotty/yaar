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

use core::{
    cell::Cell, cmp::Ordering as CmpOrdering, marker::PhantomPinned, mem::size_of, pin::Pin,
    ptr::NonNull,
};

#[derive(Default)]
pub struct WaitNode {
    _pinned: PhantomPinned,
    address: Cell<usize>,
    ticket: Cell<usize>,
    parent: Cell<Option<NonNull<Self>>>,
    children: [Cell<Option<NonNull<Self>>>; 2],
    next: Cell<Option<NonNull<Self>>>,
    prev: Cell<Option<NonNull<Self>>>,
    tail: Cell<Option<NonNull<Self>>>,
}

pub struct WaitTree {
    root: Cell<Option<NonNull<WaitNode>>>,
    prng: Cell<usize>,
}

impl WaitTree {
    pub const fn new() -> Self {
        Self {
            root: Cell::new(None),
            prng: Cell::new(0),
        }
    }

    pub fn gen_random(&self, seed: usize) -> usize {
        let shifts = match size_of::<usize>() {
            8 => (13, 7, 17),
            4 => (13, 17, 5),
            2 => (7, 9, 8),
            _ => unreachable!("Architecture not supported"),
        };

        let mut xorshift = self.prng.get();
        if xorshift == 0 {
            xorshift = seed | 1;
        }

        xorshift ^= xorshift << shifts.0;
        xorshift ^= xorshift >> shifts.1;
        xorshift ^= xorshift << shifts.2;
        self.prng.set(xorshift);

        xorshift
    }

    unsafe fn insert(&self, parent: Option<NonNull<WaitNode>>, node: NonNull<WaitNode>) {
        node.as_ref().parent.set(parent);
        node.as_ref().children[0].set(None);
        node.as_ref().children[1].set(None);

        if let Some(parent) = parent {
            let address = node.as_ref().address.get();
            let is_left = address < parent.as_ref().address.get();
            parent.as_ref().children[!is_left as usize].set(Some(node));
        } else {
            self.root.set(Some(node));
        }

        let ticket = self.gen_random(node.as_ptr() as usize) | 1;
        node.as_ref().ticket.set(ticket);
        while let Some(parent) = node.as_ref().parent.get() {
            if parent.as_ref().ticket.get() <= ticket {
                break;
            }

            let is_left = parent.as_ref().children[0].get() == Some(node);
            self.rotate(parent, is_left);
        }
    }

    unsafe fn remove(&self, node: NonNull<WaitNode>) {
        while node
            .as_ref()
            .children
            .iter()
            .any(|child| child.get().is_some())
        {
            self.rotate(node, {
                let left = node.as_ref().children[0].get();
                let right = node.as_ref().children[1].get();
                match (left, right) {
                    (Some(left), Some(right)) => {
                        let left_ticket = left.as_ref().ticket.get();
                        let right_ticket = right.as_ref().ticket.get();
                        right_ticket < left_ticket
                    }
                    (_, None) => false,
                    _ => true,
                }
            });
        }

        self.replace(node, None);
        node.as_ref().parent.set(None);
        node.as_ref().children[0].set(None);
        node.as_ref().children[1].set(None);
    }

    unsafe fn replace(&self, node: NonNull<WaitNode>, new_node: Option<NonNull<WaitNode>>) {
        if let Some(new_node) = new_node {
            new_node.as_ref().ticket.set(node.as_ref().ticket.get());
            new_node.as_ref().parent.set(node.as_ref().parent.get());
            new_node.as_ref().children[0].set(node.as_ref().children[0].get());
            new_node.as_ref().children[1].set(node.as_ref().children[1].get());
        }

        for child in node.as_ref().children.iter().filter_map(|c| c.get()) {
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
        let target = target.expect("rotating node with invalid child");
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
}

pub struct WaitQueue<'a> {
    address: usize,
    tree: &'a WaitTree,
    head: Cell<Option<NonNull<WaitNode>>>,
    parent: Cell<Option<NonNull<WaitNode>>>,
}

impl<'a> WaitQueue<'a> {
    pub unsafe fn from_addr(tree: &'a WaitTree, address: usize) -> WaitQueue<'a> {
        let mut parent = None;
        let mut current = tree.root.get();

        while let Some(node) = current {
            let is_left = match address.cmp(&node.as_ref().address.get()) {
                CmpOrdering::Less => true,
                CmpOrdering::Greater => false,
                CmpOrdering::Equal => {
                    return WaitQueue {
                        address,
                        tree,
                        head: Cell::new(Some(node)),
                        parent: Cell::new(parent),
                    }
                }
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

    pub unsafe fn from_node(tree: &'a WaitTree, node: Pin<&WaitNode>) -> WaitQueue<'a> {
        let address = node.address.get();

        if node.prev.get().is_none() {
            return WaitQueue {
                address,
                tree,
                head: Cell::new(Some(NonNull::from(&*node))),
                parent: Cell::new(node.parent.get()),
            };
        }

        Self::from_addr(tree, address)
    }

    pub fn is_empty(&self) -> bool {
        self.head.get().is_none()
    }

    pub unsafe fn insert(&self, node: Pin<&WaitNode>) {
        let node = NonNull::from(&*node);
        node.as_ref().prev.set(None);
        node.as_ref().next.set(None);
        node.as_ref().tail.set(Some(node));
        node.as_ref().address.set(self.address);

        if let Some(head) = self.head.get() {
            let tail = head.as_ref().tail.get();
            let tail = tail.expect("queue without tail");
            tail.as_ref().next.set(Some(node));
            node.as_ref().prev.set(Some(tail));
            head.as_ref().tail.set(Some(node));
            return;
        }

        self.tree.insert(self.parent.get(), node);
        self.head.set(Some(node));
    }

    pub unsafe fn iter(&self) -> impl Iterator<Item = NonNull<WaitNode>> {
        struct WaitQueueIter {
            current: Option<NonNull<WaitNode>>,
        }

        impl Iterator for WaitQueueIter {
            type Item = NonNull<WaitNode>;

            fn next(&mut self) -> Option<Self::Item> {
                self.current.map(|node| unsafe {
                    self.current = node.as_ref().next.get();
                    node
                })
            }
        }

        WaitQueueIter {
            current: self.head.get(),
        }
    }

    pub unsafe fn remove(&self, node: Pin<&WaitNode>) {
        let node = NonNull::from(&*node);
        let prev = node.as_ref().prev.get();
        let next = node.as_ref().next.get();
        assert_eq!(node.as_ref().tail.get(), Some(node));
        assert_eq!(node.as_ref().address.get(), self.address);

        let head = self.head.get().expect("remove on empty queue");
        if let Some(prev) = prev {
            prev.as_ref().next.set(next);
            if let Some(next) = next {
                next.as_ref().prev.set(Some(prev));
            } else {
                head.as_ref().tail.set(Some(prev));
            }
        } else {
            assert_eq!(head, node);
            self.head.set(next);
            if let Some(new_head) = next {
                new_head.as_ref().tail.set(head.as_ref().tail.get());
                self.tree.replace(head, Some(new_head));
            } else {
                self.tree.remove(head);
            }
        }

        node.as_ref().prev.set(None);
        node.as_ref().next.set(None);
        node.as_ref().tail.set(None);
    }
}
