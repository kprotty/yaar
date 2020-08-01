// Copyright 2019-2020 kprotty
//
// Licensed under the Apache License, Version 2.0, <LICENSE-APACHE or
// http://apache.org/licenses/LICENSE-2.0> or the MIT license <LICENSE-MIT or
// http://opensource.org/licenses/MIT>, at your option. This file may not be
// copied, modified, or distributed except according to those terms.

use core::{
    fmt,
    marker::{PhantomData, PhantomPinned},
    pin::Pin,
    ptr::NonNull,
};

/// A node cluster represents an ordered collection of nodes that can be
/// scheduled together.
///
/// # Safety:
///
/// Once a node is enqueued onto a NodeCluster, it must not be invalidated or
/// modified until it is either dequeued from the NodeCluster or the NodeCluster
/// is dropped. Enqueue'ing a Node into a NodeCluster, running a node, and
/// scheduling a Node count as forms of modification.
#[repr(C)]
pub struct NodeCluster {
    head: Option<NonNull<Node>>,
    tail: NonNull<Node>,
    size: usize,
}

impl fmt::Debug for NodeCluster {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("NodeCluster")
            .field("is_empty", &(self.size == 0))
            .finish()
    }
}

impl From<Pin<&mut Node>> for NodeCluster {
    fn from(node: Pin<&mut Node>) -> Self {
        // Convert the node into a NonNull<Node> to use for NodeCluster operations.
        // Safety: upholds the Pin contract as the node reference is never invalidated.
        let node = unsafe {
            let mut node = NonNull::from(Pin::get_unchecked_mut(node));
            node.as_mut().next = Some(node);
            node
        };

        Self {
            head: Some(node),
            tail: node,
            size: 1,
        }
    }
}

impl Default for NodeCluster {
    fn default() -> Self {
        Self::new()
    }
}

impl NodeCluster {
    /// Create an empty cluster of nodes
    pub const fn new() -> Self {
        Self {
            head: None,
            tail: NonNull::dangling(),
            size: 0,
        }
    }

    /// Returns the amount of nodes currently present in the node cluster
    #[inline]
    pub fn len(&self) -> usize {
        self.size
    }

    /// Enqueue a node or cluster of nodes to the end of this node cluster.
    ///
    /// # Safety:
    ///
    /// This is effectively an alias for [`push_back`] and requires the caller
    /// to uphold the same invariants.
    #[inline]
    pub unsafe fn push(&mut self, cluster: impl Into<Self>) {
        self.push_back(cluster)
    }

    /// Enqueue a node or cluster of nodes to the end of this node cluster.
    ///
    /// # Safety:
    ///
    /// Refer to the safety invariants listed at the documentation for
    /// [`NodeCluster`]
    pub unsafe fn push_back(&mut self, cluster: impl Into<Self>) {
        let mut cluster = cluster.into();
        if cluster.head.is_some() {
            if self.head.is_some() {
                cluster.tail.as_mut().next = self.head;
                self.tail.as_mut().next = cluster.head;
                self.tail = cluster.tail;
                self.size += cluster.size;
            } else {
                *self = cluster;
            }
        }
    }

    /// Enqueue a node or cluster of nodes to the beginning of this node
    /// cluster.
    ///
    /// # Safety:
    ///
    /// Refer to the safety invariants listed at the documentation for
    /// [`NodeCluster`]
    pub unsafe fn push_front(&mut self, cluster: impl Into<Self>) {
        let mut cluster = cluster.into();
        if cluster.head.is_some() {
            if self.head.is_some() {
                cluster.tail.as_mut().next = self.head;
                self.tail.as_mut().next = cluster.head;
                self.head = cluster.head;
                self.size += cluster.size;
            } else {
                *self = cluster;
            }
        }
    }

    /// Dequeue a node from the beginning of this node cluster.
    ///
    /// # Safety:
    ///
    /// This is effectively an alias for [`pop_front`] and requires the caller
    /// to uphold the same invariants.
    #[inline]
    pub unsafe fn pop(&mut self) -> Option<NonNull<Node>> {
        self.pop_front()
    }

    /// Dequeue a node from the beginning of this node cluster.
    ///
    /// # Safety:
    ///
    /// Refer to the safety invariants listed at the documentation for
    /// [`NodeCluster`]
    pub unsafe fn pop_front(&mut self) -> Option<NonNull<Node>> {
        self.head.map(|mut node| {
            self.head = node.as_ref().next;
            self.tail.as_mut().next = Some(self.head.unwrap_or(self.tail));
            self.size -= 1;

            node.as_mut().next = Some(node);
            node
        })
    }

    /// Iterate the [`Node`]s in the order that they were added to this
    /// [`NodeCluster`].
    pub fn iter<'a>(&self) -> NodeIter<'a> {
        NodeIter::new(self.head)
    }
}

/// Used to iterate all the [`Node`]s that are connected in a [`NodeCluster`].
pub struct NodeIter<'a> {
    start: Option<NonNull<Node>>,
    current: Option<NonNull<Node>>,
    _lifetime: PhantomData<&'a ()>,
}

impl<'a> fmt::Debug for NodeIter<'a> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("NodeIter").finish()
    }
}

impl<'a> NodeIter<'a> {
    const fn new(start: Option<NonNull<Node>>) -> Self {
        Self {
            start,
            current: start,
            _lifetime: PhantomData,
        }
    }
}

impl<'a> Iterator for NodeIter<'a> {
    type Item = NonNull<Node>;

    fn next(&mut self) -> Option<Self::Item> {
        let node = self.current?;
        self.current = unsafe { node.as_ref().next };
        if self.current == self.start {
            self.current = None;
        }
        Some(node)
    }
}

/// TODO: documentation for a Node
pub struct Node {
    _pinned: PhantomPinned,
    next: Option<NonNull<Self>>,
}

impl fmt::Debug for Node {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Node").finish()
    }
}

impl Default for Node {
    fn default() -> Self {
        Self {
            _pinned: PhantomPinned,
            next: None,
        }
    }
}

impl Node {
    /// Returns an iterator used to visit all the [`Node`]s in the
    /// [`NodeCluster`] that this Node belongs to, starting from this node
    /// itself
    pub fn iter<'a>(&'a self) -> NodeIter<'a> {
        NodeIter::new(Some(NonNull::from(self)))
    }
}
