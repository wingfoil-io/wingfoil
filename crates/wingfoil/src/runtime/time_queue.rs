use std::collections::btree_map::Entry;
use std::collections::{BTreeMap, VecDeque};

use crate::runtime::time::NanoTime;

/// The entries queued at one instant. Overwhelmingly there is exactly one — two
/// nodes scheduled for the very same nanosecond is the exception — so the
/// single-entry case is held inline. A `Vec`-per-instant instead put a heap
/// allocation and a free on the path of every schedule, which is most of what
/// one costs.
#[derive(Debug)]
enum Bucket<T> {
    One(T),
    /// Two or more distinct values at this instant, in FIFO order. Never empty
    /// and never of length one: `pop` collapses it back to `One` at two.
    Many(VecDeque<T>),
}

impl<T> Bucket<T> {
    /// Append `value` behind everything already queued at this instant.
    fn push_back(&mut self, value: T) {
        match self {
            Bucket::Many(q) => q.push_back(value),
            Bucket::One(_) => {
                let Bucket::One(first) = std::mem::replace(self, Bucket::Many(VecDeque::new()))
                else {
                    unreachable!("invariant: matched Bucket::One immediately above")
                };
                let Bucket::Many(q) = self else {
                    unreachable!("invariant: just replaced with Bucket::Many")
                };
                q.push_back(first);
                q.push_back(value);
            }
        }
    }

    /// Take the front (earliest-pushed) value, and report whether anything is
    /// still queued at this instant. `false` means the bucket is spent and its
    /// instant should retire — the no-empty-bucket invariant.
    fn pop_front(&mut self) -> (T, bool) {
        match self {
            Bucket::Many(q) => {
                let value = q
                    .pop_front()
                    .expect("invariant: Bucket::Many is never empty");
                if q.len() == 1 {
                    // Collapse back to the inline form so a long-lived queue
                    // does not keep a `VecDeque` per instant alive.
                    let last = q.pop_front().expect("invariant: len checked as 1");
                    *self = Bucket::One(last);
                }
                (value, true)
            }
            Bucket::One(_) => {
                // The empty `Many` left behind is transient: the caller drops
                // the whole bucket on the `false`.
                let Bucket::One(value) = std::mem::replace(self, Bucket::Many(VecDeque::new()))
                else {
                    unreachable!("invariant: matched Bucket::One immediately above")
                };
                (value, false)
            }
        }
    }
}

impl<T: PartialEq> Bucket<T> {
    /// Whether `value` is already queued at this instant — the dedup test,
    /// scoped to one bucket.
    fn contains(&self, value: &T) -> bool {
        match self {
            Bucket::One(queued) => queued == value,
            Bucket::Many(queued) => queued.iter().any(|q| q == value),
        }
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
/// ## Why the entries are bucketed by time
///
/// Dedup only ever compares against entries sharing the *same* `time`, so the
/// queue keys entries by time rather than holding one flat heap: a push scans
/// its own instant's bucket (almost always empty or a single entry) instead of
/// every pending item.
///
/// That distinction is on the hot path, not a micro-optimisation. This queue
/// backs the graph scheduler, where **every scheduling node holds a pending
/// entry at all times** — a ticker re-arms itself the moment it fires — so the
/// pending set is the size of the graph's timer population, not "a handful".
/// A flat scan made one `Ctx::schedule` cost `O(pending)`: measured at 22 ns
/// with one entry queued and 686 ns with 1024, i.e. every timer in the graph
/// taxing every other timer's tick. `delay` pays the same shape, its pending
/// set being every value still in flight.
///
/// The earliest instant is then held *out* of the map, in `front`, and refilled
/// **lazily** — `pop` leaves `front` empty rather than promoting the map's next
/// instant, and a later `push` earlier than that instant claims the empty
/// `front` directly. A `BTreeMap` insert and remove costs ~30 ns together, and
/// allocates and frees a root node every time the map empties and refills, so
/// the two shapes that matter must not touch it at all:
///
/// * **one timer** — the map stays empty for the whole run; `front` alone
///   absorbs every fire-and-re-arm;
/// * **one fast timer among slow ones** — the fast timer's next fire is earlier
///   than everything in the map, so it lands in the vacated `front` while the
///   slow instants sit undisturbed. Eager promotion made this the *worst* case
///   instead: the map thrashed between one entry and none on every cycle.
///
/// None of this is observable: earliest time first, FIFO within a time
/// (insertion order), and `(value, time)` dedup — all exactly as before.
#[derive(Debug)]
pub struct TimeQueue<T> {
    /// The earliest pending instant and its entries, held out of `buckets`.
    /// **Invariant: when occupied, `front.0` is strictly less than every key in
    /// `buckets`** — so entries sharing an instant are never split across the
    /// two, and an occupied `front` answers `next_time` outright. `None` does
    /// *not* mean the queue is empty: the earliest instant may still be sitting
    /// in `buckets`, waiting for a `pop` or an earlier `push` to claim the slot.
    front: Option<(NanoTime, Bucket<T>)>,
    /// Every later instant, earliest key first; within a bucket, insertion
    /// (FIFO) order. **Invariant: no bucket is ever empty.**
    buckets: BTreeMap<NanoTime, Bucket<T>>,
}

impl<T> Default for TimeQueue<T> {
    fn default() -> Self {
        Self {
            front: None,
            buckets: BTreeMap::new(),
        }
    }
}

impl<T> TimeQueue<T> {
    pub fn new() -> Self {
        Self::default()
    }

    /// Time of the next item, or `None` if the queue is empty.
    pub fn next_time(&self) -> Option<NanoTime> {
        match self.front {
            Some((time, _)) => Some(time),
            // `front` vacated but the map still holds instants — see the field's
            // invariant. Reading the minimum here (rather than promoting it)
            // keeps the map untouched.
            None => self.buckets.first_key_value().map(|(&time, _)| time),
        }
    }

    pub fn is_empty(&self) -> bool {
        self.front.is_none() && self.buckets.is_empty()
    }

    /// Pop the earliest item, or `None` if the queue is empty.
    pub fn pop(&mut self) -> Option<T> {
        if self.front.is_none() {
            // Only now is promoting worth a map operation: something is being
            // taken out, so the instant is about to be consumed anyway.
            self.front = self.buckets.pop_first();
        }
        let (_, bucket) = self.front.as_mut()?;
        let (value, instant_still_occupied) = bucket.pop_front();
        if !instant_still_occupied {
            self.front = None;
        }
        Some(value)
    }

    /// Pop the earliest item iff its time is `<= current_time`. Designed for the
    /// `while let Some(v) = q.pop_if_pending(now) { ... }` idiom that drains all
    /// callbacks due at or before the current engine tick.
    ///
    /// The common call is the one that finds *nothing* due — the drain loop's
    /// last iteration, every cycle — so the not-due answer must not touch the
    /// map beyond reading its minimum key.
    pub fn pop_if_pending(&mut self, current_time: NanoTime) -> Option<T> {
        match self.next_time() {
            Some(time) if time <= current_time => self.pop(),
            _ => None,
        }
    }

    pub fn clear(&mut self) {
        self.front = None;
        self.buckets.clear();
    }
}

impl<T: PartialEq> TimeQueue<T> {
    /// Push `value` at `time`. A `(value, time)` pair already present in the
    /// queue is suppressed (see the type-level docs — dedup is intentional).
    pub fn push(&mut self, value: T, time: NanoTime) {
        // Only entries at the *same* time can be duplicates, so the scan is
        // confined to this instant's bucket.
        match self.front.as_mut() {
            // Same instant as the held-out front: the whole operation stays out
            // of the map. This is the single-timer graph's every push.
            Some((front_time, bucket)) if *front_time == time => {
                if bucket.contains(&value) {
                    return;
                }
                bucket.push_back(value);
            }
            // Earlier than the front: it becomes the new front, and the old one
            // moves into the map (whose keys are all later still, so the
            // strictly-less-than invariant is preserved).
            Some((front_time, _)) if time < *front_time => {
                let Some((old_time, old_bucket)) = self.front.replace((time, Bucket::One(value)))
                else {
                    unreachable!("invariant: matched Some immediately above")
                };
                self.buckets.insert(old_time, old_bucket);
            }
            // A later instant, with the front occupied: it belongs in the map.
            Some(_) => self.push_into_map(value, time),
            // `front` is vacant. Claim it if this instant is earlier than
            // everything in the map — the fast-timer-among-slow-ones case, which
            // must not cost a map mutation.
            None => match self.buckets.first_key_value() {
                Some((&earliest, _)) if time >= earliest => self.push_into_map(value, time),
                _ => self.front = Some((time, Bucket::One(value))),
            },
        }
    }

    /// Push into `buckets`, deduplicating within the instant's bucket. One
    /// descent of the map, not a `get_mut` followed by an `insert`.
    fn push_into_map(&mut self, value: T, time: NanoTime) {
        match self.buckets.entry(time) {
            Entry::Occupied(mut bucket) => {
                if bucket.get().contains(&value) {
                    return;
                }
                bucket.get_mut().push_back(value);
            }
            Entry::Vacant(slot) => {
                slot.insert(Bucket::One(value));
            }
        }
    }
}

#[cfg(test)]
mod tests {

    use super::TimeQueue;
    use crate::runtime::time::NanoTime;

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
    fn draining_a_time_lets_it_be_pushed_again() {
        // The no-empty-bucket invariant: popping the last entry at a time must
        // retire that time entirely, so a later push at the same time is a
        // fresh entry rather than a dedup hit against a drained ghost.
        let mut queue: TimeQueue<u32> = TimeQueue::new();
        queue.push(1, NanoTime::new(100));
        assert_eq!(queue.pop(), Some(1));
        assert!(queue.is_empty());
        assert_eq!(queue.next_time(), None);
        queue.push(1, NanoTime::new(100));
        assert_eq!(queue.next_time(), Some(NanoTime::new(100)));
        assert_eq!(queue.pop(), Some(1));
    }

    #[test]
    fn dedup_is_scoped_to_the_pushed_time_only() {
        // Two entries at t=100 and one at t=200. Re-pushing `1` at 200 must be
        // suppressed (it is queued there) while `2` at 200 is accepted, even
        // though `2` is already queued at 100.
        let mut queue: TimeQueue<u32> = TimeQueue::new();
        queue.push(1, NanoTime::new(100));
        queue.push(2, NanoTime::new(100));
        queue.push(1, NanoTime::new(200));
        queue.push(1, NanoTime::new(200)); // duplicate → suppressed
        queue.push(2, NanoTime::new(200));
        assert_eq!(queue.pop(), Some(1));
        assert_eq!(queue.pop(), Some(2));
        assert_eq!(queue.pop(), Some(1));
        assert_eq!(queue.pop(), Some(2));
        assert!(queue.is_empty());
    }

    #[test]
    fn a_fast_repeat_interleaves_correctly_with_a_distant_entry() {
        // The lazy-refill shape: one distant entry parked in the map while a
        // fast one is popped and re-pushed ahead of it, over and over. The
        // vacated front must be reclaimed by each re-push (never letting the
        // distant entry jump the queue), and the distant entry must still come
        // out last, exactly once.
        let mut queue: TimeQueue<u32> = TimeQueue::new();
        queue.push(99, NanoTime::new(1_000_000));
        queue.push(1, NanoTime::new(10));
        for step in 1..=5 {
            assert_eq!(queue.next_time(), Some(NanoTime::new(step * 10)));
            assert_eq!(queue.pop(), Some(1));
            // Front is vacant and the map still holds the distant entry; this
            // push must claim the front rather than land behind it.
            queue.push(1, NanoTime::new((step + 1) * 10));
            assert_eq!(queue.next_time(), Some(NanoTime::new((step + 1) * 10)));
        }
        assert_eq!(queue.pop(), Some(1));
        assert_eq!(queue.next_time(), Some(NanoTime::new(1_000_000)));
        assert_eq!(queue.pop(), Some(99));
        assert!(queue.is_empty());
        assert_eq!(queue.pop(), None);
    }

    #[test]
    fn is_empty_is_false_while_only_the_map_holds_entries() {
        // A vacated `front` is not an empty queue: `pop` and `next_time` must
        // still see what is parked in the map behind it.
        let mut queue: TimeQueue<u32> = TimeQueue::new();
        queue.push(1, NanoTime::new(100));
        queue.push(2, NanoTime::new(200));
        assert_eq!(queue.pop(), Some(1)); // vacates `front`; 2 stays in the map
        assert!(!queue.is_empty());
        assert_eq!(queue.next_time(), Some(NanoTime::new(200)));
        assert_eq!(queue.pop_if_pending(NanoTime::new(199)), None);
        assert_eq!(queue.pop_if_pending(NanoTime::new(200)), Some(2));
        assert!(queue.is_empty());
    }

    #[test]
    fn dedup_holds_against_an_entry_parked_in_the_map() {
        // Dedup must not depend on where an entry happens to live. `3` is in the
        // map (behind the front), so re-pushing it at the same time is still a
        // duplicate — and stays one after the front is vacated.
        let mut queue: TimeQueue<u32> = TimeQueue::new();
        queue.push(1, NanoTime::new(100));
        queue.push(3, NanoTime::new(300));
        queue.push(3, NanoTime::new(300)); // duplicate, front occupied
        assert_eq!(queue.pop(), Some(1)); // vacates `front`
        queue.push(3, NanoTime::new(300)); // duplicate, front vacant
        assert_eq!(queue.pop(), Some(3));
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
