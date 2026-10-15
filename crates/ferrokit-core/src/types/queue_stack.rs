// SPDX-License-Identifier: LGPL-3.0-only
//! Queue and Stack containers matching Qt's `QQueue` and `QStack`.

use std::collections::VecDeque;
use std::ops::{Deref, DerefMut};

/// A FIFO (first-in, first-out) queue container, modeled after Qt's `QQueue`.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Default)]
pub struct Queue<T>(pub VecDeque<T>);

impl<T> Queue<T> {
    /// Creates an empty queue.
    pub fn new() -> Self {
        Self(VecDeque::new())
    }

    /// Creates an empty queue with the specified capacity.
    pub fn with_capacity(capacity: usize) -> Self {
        Self(VecDeque::with_capacity(capacity))
    }

    /// Inserts `value` at the tail of the queue, matching `QQueue::enqueue`.
    pub fn enqueue(&mut self, value: T) {
        self.0.push_back(value);
    }

    /// Removes and returns the value at the head of the queue, matching `QQueue::dequeue`.
    pub fn dequeue(&mut self) -> Option<T> {
        self.0.pop_front()
    }

    /// Returns a reference to the value at the head of the queue without removing it,
    /// matching `QQueue::head`.
    pub fn head(&self) -> Option<&T> {
        self.0.front()
    }

    /// Returns a mutable reference to the value at the head of the queue,
    /// matching `QQueue::head`.
    pub fn head_mut(&mut self) -> Option<&mut T> {
        self.0.front_mut()
    }

    /// Consumes the queue and returns the underlying [`VecDeque`].
    pub fn into_inner(self) -> VecDeque<T> {
        self.0
    }
}

impl<T> Deref for Queue<T> {
    type Target = VecDeque<T>;

    fn deref(&self) -> &Self::Target {
        &self.0
    }
}

impl<T> DerefMut for Queue<T> {
    fn deref_mut(&mut self) -> &mut Self::Target {
        &mut self.0
    }
}

impl<T> From<VecDeque<T>> for Queue<T> {
    fn from(deque: VecDeque<T>) -> Self {
        Self(deque)
    }
}

impl<T> From<Vec<T>> for Queue<T> {
    fn from(vec: Vec<T>) -> Self {
        Self(VecDeque::from(vec))
    }
}

impl<T> From<Queue<T>> for VecDeque<T> {
    fn from(q: Queue<T>) -> Self {
        q.0
    }
}

impl<T> FromIterator<T> for Queue<T> {
    fn from_iter<I: IntoIterator<Item = T>>(iter: I) -> Self {
        Self(iter.into_iter().collect())
    }
}

impl<T> IntoIterator for Queue<T> {
    type Item = T;
    type IntoIter = std::collections::vec_deque::IntoIter<T>;

    fn into_iter(self) -> Self::IntoIter {
        self.0.into_iter()
    }
}

impl<'a, T> IntoIterator for &'a Queue<T> {
    type Item = &'a T;
    type IntoIter = std::collections::vec_deque::Iter<'a, T>;

    fn into_iter(self) -> Self::IntoIter {
        self.0.iter()
    }
}

impl<'a, T> IntoIterator for &'a mut Queue<T> {
    type Item = &'a mut T;
    type IntoIter = std::collections::vec_deque::IterMut<'a, T>;

    fn into_iter(self) -> Self::IntoIter {
        self.0.iter_mut()
    }
}

impl<T> Extend<T> for Queue<T> {
    fn extend<I: IntoIterator<Item = T>>(&mut self, iter: I) {
        self.0.extend(iter);
    }
}

/// A LIFO (last-in, first-out) stack container, modeled after Qt's `QStack`.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Default)]
pub struct Stack<T>(pub Vec<T>);

impl<T> Stack<T> {
    /// Creates an empty stack.
    pub fn new() -> Self {
        Self(Vec::new())
    }

    /// Creates an empty stack with the specified capacity.
    pub fn with_capacity(capacity: usize) -> Self {
        Self(Vec::with_capacity(capacity))
    }

    /// Pushes `value` onto the top of the stack, matching `QStack::push`.
    pub fn push(&mut self, value: T) {
        self.0.push(value);
    }

    /// Pops and returns the top value from the stack, matching `QStack::pop`.
    pub fn pop(&mut self) -> Option<T> {
        self.0.pop()
    }

    /// Returns a reference to the top value on the stack, matching `QStack::top`.
    pub fn top(&self) -> Option<&T> {
        self.0.last()
    }

    /// Returns a mutable reference to the top value on the stack, matching `QStack::top`.
    pub fn top_mut(&mut self) -> Option<&mut T> {
        self.0.last_mut()
    }

    /// Consumes the stack and returns the underlying [`Vec`].
    pub fn into_inner(self) -> Vec<T> {
        self.0
    }
}

impl<T> Deref for Stack<T> {
    type Target = [T];

    fn deref(&self) -> &Self::Target {
        &self.0
    }
}

impl<T> DerefMut for Stack<T> {
    fn deref_mut(&mut self) -> &mut Self::Target {
        &mut self.0
    }
}

impl<T> From<Vec<T>> for Stack<T> {
    fn from(vec: Vec<T>) -> Self {
        Self(vec)
    }
}

impl<T> From<Stack<T>> for Vec<T> {
    fn from(s: Stack<T>) -> Self {
        s.0
    }
}

impl<T> FromIterator<T> for Stack<T> {
    fn from_iter<I: IntoIterator<Item = T>>(iter: I) -> Self {
        Self(iter.into_iter().collect())
    }
}

impl<T> IntoIterator for Stack<T> {
    type Item = T;
    type IntoIter = std::vec::IntoIter<T>;

    fn into_iter(self) -> Self::IntoIter {
        self.0.into_iter()
    }
}

impl<'a, T> IntoIterator for &'a Stack<T> {
    type Item = &'a T;
    type IntoIter = std::slice::Iter<'a, T>;

    fn into_iter(self) -> Self::IntoIter {
        self.0.iter()
    }
}

impl<'a, T> IntoIterator for &'a mut Stack<T> {
    type Item = &'a mut T;
    type IntoIter = std::slice::IterMut<'a, T>;

    fn into_iter(self) -> Self::IntoIter {
        self.0.iter_mut()
    }
}

impl<T> Extend<T> for Stack<T> {
    fn extend<I: IntoIterator<Item = T>>(&mut self, iter: I) {
        self.0.extend(iter);
    }
}
