//! Adapters to convert Iterators into Streams

use crate::queue::ValueAt;
use crate::types::*;
use anyhow::anyhow;

use std::cmp::Ordering;

type Peeker<T> = std::iter::Peekable<Box<dyn Iterator<Item = ValueAt<T>>>>;

/// Wraps an Iterator and exposes it as a [`Stream`] of [`Burst<T>`].
/// Multiple items with the same timestamp are grouped into a single [`Burst`] per tick.
pub struct IteratorStream<T: Element> {
    peekable: Peeker<T>,
    value: Burst<T>,
}

fn add_callback<T>(peekable: &mut Peeker<T>, state: &mut GraphState) -> anyhow::Result<bool> {
    match peekable.peek() {
        Some(value_at) => {
            state.add_callback(value_at.time);
            Ok(true)
        }
        None => Ok(false),
    }
}

#[node(output = value: Burst<T>)]
impl<T: Element> MutableNode for IteratorStream<T> {
    fn cycle(&mut self, state: &mut GraphState) -> anyhow::Result<bool> {
        self.value.clear();
        while let Some(value_at) = self.peekable.peek() {
            if value_at.time == state.time() {
                // peek() returned Some, so next() is guaranteed.
                let val = self
                    .peekable
                    .next()
                    .expect("peek() just returned Some")
                    .value
                    .clone();
                self.value.push(val);
            } else {
                break;
            }
        }
        add_callback(&mut self.peekable, state)
    }

    fn start(&mut self, state: &mut GraphState) -> anyhow::Result<()> {
        add_callback(&mut self.peekable, state)?;
        Ok(())
    }
}

impl<T> IteratorStream<T>
where
    T: Element + 'static,
{
    pub fn new(it: Box<dyn Iterator<Item = ValueAt<T>>>) -> Self {
        Self {
            peekable: it.peekable(),
            value: Burst::new(),
        }
    }
}

type TryPeeker<T> = std::iter::Peekable<Box<dyn Iterator<Item = anyhow::Result<ValueAt<T>>>>>;

/// Like [`IteratorStream`] but for a fallible iterator: each item is an
/// `anyhow::Result<ValueAt<T>>`. A row that resolves to `Err` is surfaced to
/// the graph (failing the run) the moment the stream reaches it, instead of
/// panicking. Successful items are grouped into a [`Burst<T>`] per tick, as in
/// [`IteratorStream`].
pub struct TryIteratorStream<T: Element> {
    peekable: TryPeeker<T>,
    value: Burst<T>,
}

/// Schedule a callback for the next item, or surface its error.
///
/// If the next item is `Ok`, a callback is scheduled at its time. If it is
/// `Err`, the error is consumed and returned (we cannot know its time, so it is
/// surfaced as soon as it reaches the head of the iterator). Returns `Ok(false)`
/// when the iterator is exhausted.
fn try_add_callback<T: Element>(
    peekable: &mut TryPeeker<T>,
    state: &mut GraphState,
) -> anyhow::Result<bool> {
    match peekable.peek() {
        Some(Ok(value_at)) => {
            let time = value_at.time;
            state.add_callback(time);
            Ok(true)
        }
        Some(Err(_)) => Err(peekable
            .next()
            .expect("peek() just returned Some")
            .expect_err("peek() just returned Err")),
        None => Ok(false),
    }
}

#[node(output = value: Burst<T>)]
impl<T: Element> MutableNode for TryIteratorStream<T> {
    fn cycle(&mut self, state: &mut GraphState) -> anyhow::Result<bool> {
        self.value.clear();
        while let Some(item) = self.peekable.peek() {
            let due = match item {
                Ok(value_at) => value_at.time == state.time(),
                Err(_) => false,
            };
            if !due {
                break;
            }
            // peek() returned Some(Ok), so next() yields the same Ok item.
            let value_at = self
                .peekable
                .next()
                .expect("peek() just returned Some")
                .expect("peek() just returned Ok");
            self.value.push(value_at.value);
        }
        try_add_callback(&mut self.peekable, state)
    }

    fn start(&mut self, state: &mut GraphState) -> anyhow::Result<()> {
        try_add_callback(&mut self.peekable, state)?;
        Ok(())
    }
}

impl<T> TryIteratorStream<T>
where
    T: Element + 'static,
{
    pub fn new(it: Box<dyn Iterator<Item = anyhow::Result<ValueAt<T>>>>) -> Self {
        Self {
            peekable: it.peekable(),
            value: Burst::new(),
        }
    }
}

/// Wraps an Iterator and exposes it as a [Stream] of values.
/// The source must be strictly ascending in time. If the source
/// can tick multiple times at the same timestamp, use
/// [IteratorStream] instead, which emits [`Burst<T>`].
pub struct SimpleIteratorStream<T: Element> {
    peekable: Peeker<T>,
    value: T,
}

#[node(output = value: T)]
impl<T: Element> MutableNode for SimpleIteratorStream<T> {
    fn cycle(&mut self, state: &mut GraphState) -> anyhow::Result<bool> {
        // Scheduling only happens when peek() is Some, so cycle() can only be
        // entered if the iterator has at least one item left.
        let val_at1 = self
            .peekable
            .next()
            .expect("SimpleIteratorStream cycled with no upcoming item");
        self.value = val_at1.value;

        if let Some(val_at2) = self.peekable.peek() {
            match val_at1.time.cmp(&val_at2.time) {
                Ordering::Greater => {
                    return Err(anyhow!("source time was descending!"));
                }
                Ordering::Equal => {
                    return Err(anyhow!(
                        "source produced multiple ticks for same time, use IteratorStream instead"
                    ));
                }
                Ordering::Less => {}
            }
        }
        add_callback(&mut self.peekable, state)
    }

    fn start(&mut self, state: &mut GraphState) -> anyhow::Result<()> {
        add_callback(&mut self.peekable, state)?;
        Ok(())
    }
}

impl<T> SimpleIteratorStream<T>
where
    T: Element + 'static,
{
    pub fn new(it: Box<dyn Iterator<Item = ValueAt<T>>>) -> SimpleIteratorStream<T> {
        Self {
            peekable: it.peekable(),
            value: T::default(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::graph::*;
    use crate::nodes::*;
    use crate::types::IntoStream;

    fn value_ats(pairs: &[(u64, u64)]) -> Vec<ValueAt<u64>> {
        pairs
            .iter()
            .map(|&(v, t)| ValueAt {
                value: v,
                time: NanoTime::new(t),
            })
            .collect()
    }

    #[test]
    fn simple_iterator_emits_in_order() {
        // SimpleIteratorStream doesn't emit the final item (it acts as the scheduler
        // for the previous item). Add a sentinel at t=300 so all "real" items emit.
        let items = value_ats(&[(10, 0), (20, 100), (30, 200), (0, 300)]);
        let out = SimpleIteratorStream::new(Box::new(items.into_iter()))
            .into_stream()
            .collect();
        out.run(RunMode::HistoricalFrom(NanoTime::ZERO), RunFor::Forever)
            .unwrap();
        let values: Vec<u64> = out.peek_value().iter().map(|v| v.value).collect();
        // Sentinel (0 at t=300) schedules t=300 but returns Ok(false), so it doesn't emit.
        assert_eq!(values, vec![10, 20, 30]);
    }

    #[test]
    fn iterator_stream_groups_same_timestamp_into_burst() {
        // Two items at t=0, one at t=100, plus a sentinel at t=200.
        // IteratorStream doesn't emit the last group (it schedules the previous one).
        let items = value_ats(&[(1, 0), (2, 0), (3, 100), (0, 200)]);
        let out = IteratorStream::new(Box::new(items.into_iter()))
            .into_stream()
            .collect();
        out.run(RunMode::HistoricalFrom(NanoTime::ZERO), RunFor::Forever)
            .unwrap();
        let ticks = out.peek_value();
        assert_eq!(ticks.len(), 2);
        assert_eq!(ticks[0].value.as_slice(), &[1u64, 2u64]);
        assert_eq!(ticks[1].value.as_slice(), &[3u64]);
    }

    #[test]
    fn try_iterator_groups_ok_items_into_burst() {
        // Two Ok items at t=0, one at t=100, plus a sentinel at t=200.
        let items: Vec<anyhow::Result<ValueAt<u64>>> =
            value_ats(&[(1, 0), (2, 0), (3, 100), (0, 200)])
                .into_iter()
                .map(Ok)
                .collect();
        let out = TryIteratorStream::new(Box::new(items.into_iter()))
            .into_stream()
            .collect();
        out.run(RunMode::HistoricalFrom(NanoTime::ZERO), RunFor::Forever)
            .unwrap();
        let ticks = out.peek_value();
        assert_eq!(ticks.len(), 2);
        assert_eq!(ticks[0].value.as_slice(), &[1u64, 2u64]);
        assert_eq!(ticks[1].value.as_slice(), &[3u64]);
    }

    #[test]
    fn try_iterator_surfaces_item_error() {
        // An Err item must fail the run rather than panic.
        let items: Vec<anyhow::Result<ValueAt<u64>>> = vec![
            Ok(ValueAt {
                value: 1,
                time: NanoTime::new(0),
            }),
            Err(anyhow!("bad row")),
            Ok(ValueAt {
                value: 3,
                time: NanoTime::new(200),
            }),
        ];
        let result = TryIteratorStream::new(Box::new(items.into_iter()))
            .into_stream()
            .run(RunMode::HistoricalFrom(NanoTime::ZERO), RunFor::Forever);
        let err = result.expect_err("expected the item error to surface");
        assert!(format!("{err:#}").contains("bad row"), "got: {err:#}");
    }

    #[test]
    fn simple_iterator_errors_on_duplicate_timestamps() {
        let items = value_ats(&[(1, 0), (2, 0)]); // same timestamp — illegal for Simple
        let result = SimpleIteratorStream::new(Box::new(items.into_iter()))
            .into_stream()
            .run(RunMode::HistoricalFrom(NanoTime::ZERO), RunFor::Forever);
        assert!(result.is_err());
    }

    #[test]
    fn simple_iterator_errors_on_descending_timestamps() {
        let items = value_ats(&[(1, 100), (2, 50)]); // descending — illegal
        let result = SimpleIteratorStream::new(Box::new(items.into_iter()))
            .into_stream()
            .run(RunMode::HistoricalFrom(NanoTime::ZERO), RunFor::Forever);
        assert!(result.is_err());
    }
}
