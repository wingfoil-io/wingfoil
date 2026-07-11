use std::cmp::{Ordering, Reverse};
use std::collections::BinaryHeap;

use crate::types::NanoTime;

/// An entry in a [`TimeQueue`], ordered by `(time, seq)` alone.
///
/// The ordering deliberately ignores the payload `T` so the heap needs no
/// `Ord`/`Hash`/`Eq` bound on `T`. `seq` is a per-queue monotonic counter that
/// makes the order *total* (no two entries compare equal) and gives a stable
/// FIFO order among entries sharing a `time`.
#[derive(Debug)]
struct Entry<T> {
    time: NanoTime,
    seq: u64,
    value: T,
}

impl<T> Entry<T> {
    fn key(&self) -> (NanoTime, u64) {
        (self.time, self.seq)
    }
}

impl<T> PartialEq for Entry<T> {
    fn eq(&self, other: &Self) -> bool {
        self.key() == other.key()
    }
}
impl<T> Eq for Entry<T> {}
impl<T> PartialOrd for Entry<T> {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}
impl<T> Ord for Entry<T> {
    fn cmp(&self, other: &Self) -> Ordering {
        self.key().cmp(&other.key())
    }
}

/// A time-ordered queue of `T`s, earliest first.
///
/// ## Deduplication is intentional — do not remove it
///
/// Pushing a `(value, time)` pair that is **already queued** is a no-op: the
/// queue holds at most one entry per distinct `(value, time)`. This is a
/// deliberate feature, relied on by the graph scheduler and feedback machinery
/// — e.g. a node scheduled for the same instant twice, or the same feedback
/// value emitted twice in one cycle, must collapse to a single event rather
/// than fire twice. If you are tempted to "fix" duplicate suppression, it is
/// working as designed; see also `CLAUDE.md`.
///
/// Distinct values at the same `time` are all kept and pop in FIFO (insertion)
/// order.
///
/// ## Why `PartialEq`, not `Hash + Eq`
///
/// Dedup only requires comparing entries for equality, so the bound is
/// `T: PartialEq` rather than `Hash + Eq`. That matters because `f64` (and
/// other float payloads) implement `PartialEq` but neither `Hash` nor `Eq`, so
/// `delay`/`feedback`/`CallBackStream` work on float streams. (A consequence of
/// float semantics: `NaN != NaN`, so two `NaN` entries at the same time are not
/// deduplicated — which is the correct behaviour for `NaN`.)
///
/// Dedup is a linear scan of the pending entries on push; the queues this backs
/// hold only a handful of not-yet-due items at a time, so this is not a hot-path
/// concern in practice.
#[derive(Debug)]
pub(crate) struct TimeQueue<T> {
    // `BinaryHeap` is a max-heap; `Reverse` turns it into a min-heap on
    // `(time, seq)`, i.e. earliest time first, ties broken by insertion order.
    heap: BinaryHeap<Reverse<Entry<T>>>,
    next_seq: u64,
}

impl<T> Default for TimeQueue<T> {
    fn default() -> Self {
        Self {
            heap: BinaryHeap::new(),
            next_seq: 0,
        }
    }
}

impl<T> TimeQueue<T> {
    pub fn new() -> Self {
        Self::default()
    }

    /// Time of the next item, or `None` if the queue is empty.
    pub fn next_time(&self) -> Option<NanoTime> {
        self.heap.peek().map(|Reverse(e)| e.time)
    }

    pub fn is_empty(&self) -> bool {
        self.heap.is_empty()
    }

    /// Pop the earliest item, or `None` if the queue is empty.
    pub fn pop(&mut self) -> Option<T> {
        self.heap.pop().map(|Reverse(e)| e.value)
    }

    /// Pop the earliest item iff its time is `<= current_time`. Designed for the
    /// `while let Some(v) = q.pop_if_pending(now) { ... }` idiom that drains all
    /// callbacks due at or before the current engine tick.
    pub fn pop_if_pending(&mut self, current_time: NanoTime) -> Option<T> {
        match self.next_time() {
            Some(t) if t <= current_time => self.pop(),
            _ => None,
        }
    }

    pub fn clear(&mut self) {
        self.heap.clear();
    }
}

impl<T: PartialEq> TimeQueue<T> {
    /// Push `value` at `time`. A `(value, time)` pair already present in the
    /// queue is suppressed (see the type-level docs — dedup is intentional).
    pub fn push(&mut self, value: T, time: NanoTime) {
        if self
            .heap
            .iter()
            .any(|Reverse(e)| e.time == time && e.value == value)
        {
            return;
        }
        let seq = self.next_seq;
        self.next_seq += 1;
        self.heap.push(Reverse(Entry { time, seq, value }));
    }
}

#[cfg(test)]
mod tests {

    use super::TimeQueue;
    use crate::time::NanoTime;

    #[test]
    fn identical_pushes_are_deduplicated() {
        // Dedup is a feature: three identical (value, time) pushes collapse to
        // a single queued event.
        let mut queue: TimeQueue<u32> = TimeQueue::new();
        queue.push(1, NanoTime::new(100));
        queue.push(1, NanoTime::new(100));
        queue.push(1, NanoTime::new(100));
        assert_eq!(queue.pop(), Some(1));
        assert!(queue.is_empty());
    }

    #[test]
    fn same_value_distinct_times_all_kept() {
        let mut queue: TimeQueue<u32> = TimeQueue::new();
        queue.push(1, NanoTime::new(100));
        queue.push(1, NanoTime::new(200));
        queue.push(1, NanoTime::new(300));
        assert_eq!(queue.pop(), Some(1));
        assert_eq!(queue.pop(), Some(1));
        assert_eq!(queue.pop(), Some(1));
        assert!(queue.is_empty());
    }

    #[test]
    fn distinct_values_same_time_pop_in_fifo_order() {
        let mut queue: TimeQueue<u32> = TimeQueue::new();
        queue.push(1, NanoTime::new(100));
        queue.push(2, NanoTime::new(100));
        queue.push(3, NanoTime::new(100));
        // Distinct values are kept; same time → insertion (FIFO) order.
        assert_eq!(queue.pop(), Some(1));
        assert_eq!(queue.pop(), Some(2));
        assert_eq!(queue.pop(), Some(3));
        assert!(queue.is_empty());
    }

    #[test]
    fn float_payloads_are_supported_and_deduplicated() {
        // f64 is neither `Hash` nor `Eq`; the queue must still accept it, order
        // it by time, and deduplicate identical (value, time) pairs.
        let mut queue: TimeQueue<f64> = TimeQueue::new();
        queue.push(1.5, NanoTime::new(200));
        queue.push(1.5, NanoTime::new(200)); // duplicate → suppressed
        queue.push(2.5, NanoTime::new(100));
        assert_eq!(queue.pop(), Some(2.5));
        assert_eq!(queue.pop(), Some(1.5));
        assert!(queue.is_empty());
    }

    #[test]
    fn sorted() {
        let mut queue: TimeQueue<u32> = TimeQueue::new();
        queue.push(1, NanoTime::new(300));
        queue.push(3, NanoTime::new(100));
        queue.push(2, NanoTime::new(200));
        assert_eq!(queue.pop(), Some(3));
        assert_eq!(queue.pop(), Some(2));
        assert_eq!(queue.pop(), Some(1));
        assert!(queue.is_empty());
    }

    #[test]
    fn pop_if_pending() {
        let mut queue: TimeQueue<u32> = TimeQueue::new();
        assert_eq!(queue.pop_if_pending(NanoTime::MAX), None);
        queue.push(1, NanoTime::new(100));
        assert_eq!(queue.pop_if_pending(NanoTime::new(99)), None);
        assert_eq!(queue.pop_if_pending(NanoTime::new(100)), Some(1));
        assert!(queue.is_empty());
    }

    #[test]
    fn next_time_is_none_when_empty() {
        let queue: TimeQueue<u32> = TimeQueue::new();
        assert_eq!(queue.next_time(), None);
    }

    #[test]
    fn pop_is_none_when_empty() {
        let mut queue: TimeQueue<u32> = TimeQueue::new();
        assert_eq!(queue.pop(), None);
    }
}
