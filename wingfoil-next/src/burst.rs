//! [`Burst<T>`]: a group of values that occur *together* — same timestamp in
//! a historical replay, or arrived-between-cycles in realtime.
//!
//! Sources never coalesce (no latest-wins, no dropped values): when several
//! values land at one instant they are delivered as one `Burst` in a single
//! cycle, so a downstream node sees every value with its grouping intact.
//! This mirrors the classic engine's `Burst<T>` (there a `HistoricalValue`
//! carries a `ValueAt<Burst<T>>`), and is why same-time multiplicity never
//! forces the clock forward or loses data.

use std::ops::Deref;

/// A non-coalescing group of same-instant values. A source emits
/// `Stream<Burst<T>>`; fold or map over it to process every element.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Burst<T>(pub Vec<T>);

impl<T> Burst<T> {
    pub fn new() -> Self {
        Self(Vec::new())
    }

    /// A single-element burst.
    pub fn one(value: T) -> Self {
        Self(vec![value])
    }

    pub fn push(&mut self, value: T) {
        self.0.push(value);
    }

    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }

    pub fn len(&self) -> usize {
        self.0.len()
    }

    pub fn iter(&self) -> std::slice::Iter<'_, T> {
        self.0.iter()
    }
}

impl<T> Deref for Burst<T> {
    type Target = [T];
    fn deref(&self) -> &[T] {
        &self.0
    }
}

impl<T> From<Vec<T>> for Burst<T> {
    fn from(v: Vec<T>) -> Self {
        Self(v)
    }
}

impl<T> IntoIterator for Burst<T> {
    type Item = T;
    type IntoIter = std::vec::IntoIter<T>;
    fn into_iter(self) -> Self::IntoIter {
        self.0.into_iter()
    }
}

impl<'a, T> IntoIterator for &'a Burst<T> {
    type Item = &'a T;
    type IntoIter = std::slice::Iter<'a, T>;
    fn into_iter(self) -> Self::IntoIter {
        self.0.iter()
    }
}

impl<T> FromIterator<T> for Burst<T> {
    fn from_iter<I: IntoIterator<Item = T>>(iter: I) -> Self {
        Self(iter.into_iter().collect())
    }
}
