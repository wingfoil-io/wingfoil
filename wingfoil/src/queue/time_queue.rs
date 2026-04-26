use derive_new::new;
use priority_queue::PriorityQueue;
use std::cmp::Eq;
use std::cmp::Reverse;
use std::hash::Hash;

use super::value_at::ValueAt;
use crate::types::NanoTime;

/// Queue of Ts by time.
// ValueAt is used to stop duplicate values being dropped by
// PriorityQueue
#[derive(new, Default, Debug)]
pub(crate) struct TimeQueue<T: Hash + Eq> {
    #[new(default)]
    queue: PriorityQueue<ValueAt<T>, Reverse<NanoTime>>,
}

impl<T: Hash + Eq + std::fmt::Debug + std::clone::Clone> TimeQueue<T> {
    /// Time of the next item, or `None` if the queue is empty.
    pub fn next_time(&self) -> Option<NanoTime> {
        self.queue.peek().map(|(_, Reverse(t))| *t)
    }
    pub fn is_empty(&self) -> bool {
        self.queue.is_empty()
    }
    pub fn push(&mut self, value: T, time: NanoTime) {
        self.queue.push(ValueAt::new(value, time), Reverse(time));
    }
    /// Pop the earliest item, or `None` if the queue is empty.
    pub fn pop(&mut self) -> Option<T> {
        self.queue.pop().map(|(va, _)| va.value)
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
        self.queue.clear();
    }
}

#[cfg(test)]
mod tests {

    use super::TimeQueue;
    use crate::time::NanoTime;

    #[test]
    fn duplicates() {
        let mut queue: TimeQueue<u32> = TimeQueue::new();
        queue.push(1, NanoTime::new(100));
        queue.push(1, NanoTime::new(100));
        queue.push(1, NanoTime::new(100));
        assert_eq!(queue.pop(), Some(1));
        assert!(queue.is_empty());
    }

    #[test]
    fn duplicate_value() {
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
    fn duplicate_time() {
        let mut queue: TimeQueue<u32> = TimeQueue::new();
        queue.push(1, NanoTime::new(100));
        queue.push(2, NanoTime::new(100));
        queue.push(3, NanoTime::new(100));
        // 3 values in indeterminate order
        assert!(queue.pop().is_some());
        assert!(queue.pop().is_some());
        assert!(queue.pop().is_some());
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
