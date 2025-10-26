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
    pub fn next_time(&self) -> NanoTime {
        self.queue.peek().unwrap().1.0
    }
    pub fn is_empty(&self) -> bool {
        let item = self.queue.peek();
        item.is_none()
    }
    pub fn push(&mut self, value: T, time: NanoTime) {
        self.queue.push(ValueAt::new(value, time), Reverse(time));
    }
    pub fn pop(&mut self) -> T {
        self.queue.pop().unwrap().0.value
    }
    pub fn pending(&self, current_time: NanoTime) -> bool {
        let item = self.queue.peek();
        match item {
            Some(item) => item.1.0 <= current_time,
            None => false,
        }
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
        assert_eq!(queue.pop(), 1);
        assert!(queue.is_empty());
    }

    #[test]
    fn duplicate_value() {
        let mut queue: TimeQueue<u32> = TimeQueue::new();
        queue.push(1, NanoTime::new(100));
        queue.push(1, NanoTime::new(200));
        queue.push(1, NanoTime::new(300));
        assert_eq!(queue.pop(), 1);
        assert_eq!(queue.pop(), 1);
        assert_eq!(queue.pop(), 1);
        assert!(queue.is_empty());
    }

    #[test]
    fn duplicate_time() {
        let mut queue: TimeQueue<u32> = TimeQueue::new();
        queue.push(1, NanoTime::new(100));
        queue.push(2, NanoTime::new(100));
        queue.push(3, NanoTime::new(100));
        // 3 values in indeterminate order
        queue.pop();
        queue.pop();
        queue.pop();
        assert!(queue.is_empty());
    }
    #[test]
    fn sorted() {
        let mut queue: TimeQueue<u32> = TimeQueue::new();
        queue.push(1, NanoTime::new(300));
        queue.push(3, NanoTime::new(100));
        queue.push(2, NanoTime::new(200));
        assert_eq!(queue.pop(), 3);
        assert_eq!(queue.pop(), 2);
        assert_eq!(queue.pop(), 1);
        assert!(queue.is_empty());
    }
    #[test]
    fn pending() {
        let mut queue: TimeQueue<u32> = TimeQueue::new();
        assert!(!queue.pending(NanoTime::MAX));
        assert!(!queue.pending(NanoTime::new(0)));
        queue.push(1, NanoTime::new(100));
        assert!(queue.pending(NanoTime::new(100)));
        assert!(!queue.pending(NanoTime::new(99)));
    }
}
