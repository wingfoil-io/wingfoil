//! Adapters to convert Iterators into Streams

use crate::queue::ValueAt;
use crate::types::*;
use anyhow::anyhow;

use std::cmp::Ordering;

type Peeker<T> = std::iter::Peekable<Box<dyn Iterator<Item = ValueAt<T>>>>;

/// Wraps an Iterator and exposes it as a [`Stream`] of [`Burst<T>`].
/// Multiple items with the same timestamp are grouped into a single burst.
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

impl<T: Element> MutableNode for IteratorStream<T> {
    fn cycle(&mut self, state: &mut GraphState) -> anyhow::Result<bool> {
        self.value.clear();
        while let Some(value_at) = self.peekable.peek() {
            if value_at.time == state.time() {
                let val = self.peekable.next().unwrap().value.clone();
                self.value.push(val);
            } else {
                break;
            }
        }
        add_callback(&mut self.peekable, state)
    }

    fn upstreams(&self) -> UpStreams {
        UpStreams::default()
    }

    fn start(&mut self, state: &mut GraphState) -> anyhow::Result<()> {
        add_callback(&mut self.peekable, state)?;
        Ok(())
    }
}

impl<T: Element> StreamPeekRef<Burst<T>> for IteratorStream<T> {
    fn peek_ref(&self) -> &Burst<T> {
        &self.value
    }
}

impl<T> IteratorStream<T>
where
    T: Element + 'static,
{
    pub fn new(it: Box<dyn Iterator<Item = ValueAt<T>>>) -> Self {
        Self {
            peekable: it.peekable(),
            value: Burst::default(),
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

impl<T: Element> MutableNode for SimpleIteratorStream<T> {
    fn cycle(&mut self, state: &mut GraphState) -> anyhow::Result<bool> {
        let val_at1 = self.peekable.next().unwrap();
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

    fn upstreams(&self) -> UpStreams {
        UpStreams::default()
    }

    fn start(&mut self, state: &mut GraphState) -> anyhow::Result<()> {
        add_callback(&mut self.peekable, state)?;
        Ok(())
    }
}

impl<T: Element> StreamPeekRef<T> for SimpleIteratorStream<T> {
    fn peek_ref(&self) -> &T {
        &self.value
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
