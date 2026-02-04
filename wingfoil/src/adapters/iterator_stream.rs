//! Adpaters to convert an Iterators into a Streams

use crate::queue::ValueAt;
use crate::types::*;
use anyhow::anyhow;

use std::cmp::Ordering;
use std::fmt::Debug;

type Peeker<T> = Box<std::iter::Peekable<Box<dyn Iterator<Item = ValueAt<T>>>>>;

/// Wraps an Iterator and exposes it as a stream of Vectors
pub struct IteratorStream<'a, T> {
    peekable: Peeker<T>,
    value: Vec<T>,
    _phantom: std::marker::PhantomData<&'a T>,
}

fn add_callback<'a, T>(peekable: &mut Peeker<T>, state: &mut GraphState<'a>) -> anyhow::Result<bool> {
    match peekable.peek() {
        Some(value_at) => {
            state.add_callback(value_at.time);
            Ok(true)
        }
        None => Ok(false),
    }
}

impl<'a, T: Debug + Clone + 'a> MutableNode<'a> for IteratorStream<'a, T> {
    fn cycle(&mut self, state: &mut GraphState<'a>) -> anyhow::Result<bool> {
        self.value.clear();
        {
            while let Some(value_at) = self.peekable.peek() {
                if value_at.time == state.time() {
                    let val = self.peekable.next().unwrap().value.clone();
                    self.value.push(val);
                } else {
                    break;
                }
            }
        }
        add_callback(&mut self.peekable, state)
    }

    fn upstreams(&self) -> UpStreams<'a> {
        UpStreams::none()
    }

    fn start(&mut self, state: &mut GraphState<'a>) -> anyhow::Result<()> {
        add_callback(&mut self.peekable, state)?;
        Ok(())
    }
}

impl<'a, T: Debug + Clone + 'a> StreamPeekRef<'a, Vec<T>> for IteratorStream<'a, T> {
    fn peek_ref(&self) -> &Vec<T> {
        &self.value
    }
}

impl<'a, T: Debug + Clone + 'a> IteratorStream<'a, T>
{
    pub fn new(it: Box<dyn Iterator<Item = ValueAt<T>>>) -> Self {
        let peekable = Box::new(it.peekable());
        Self {
            peekable,
            value: vec![],
            _phantom: std::marker::PhantomData,
        }
    }
}

/// Wraps an Iterator and exposes it as a [Stream] of values.
/// The source must be strictly ascending in time.   If the source
/// can tick multiple times at one time, then you can use an
/// [IteratorStream] instead, which emits `Vec<T>`.
pub struct SimpleIteratorStream<'a, T> {
    peekable: Peeker<T>,
    value: T,
    _phantom: std::marker::PhantomData<&'a T>,
}

impl<'a, T: Debug + Clone + 'a> MutableNode<'a> for SimpleIteratorStream<'a, T> {
    fn cycle(&mut self, state: &mut GraphState<'a>) -> anyhow::Result<bool> {
        {
            let val_at1 = self.peekable.next().unwrap();
            self.value = val_at1.value;

            let optional_val_at2 = self.peekable.peek();
            if let Some(val_at2) = optional_val_at2 {
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
        }
        add_callback(&mut self.peekable, state)
    }

    fn upstreams(&self) -> UpStreams<'a> {
        UpStreams::none()
    }

    fn start(&mut self, state: &mut GraphState<'a>) -> anyhow::Result<()> {
        add_callback(&mut self.peekable, state)?;
        Ok(())
    }
}

impl<'a, T: Debug + Clone + 'a> StreamPeekRef<'a, T> for SimpleIteratorStream<'a, T> {
    fn peek_ref(&self) -> &T {
        &self.value
    }
}

impl<'a, T: Debug + Clone + Default + 'a> SimpleIteratorStream<'a, T>
{
    pub fn new(it: Box<dyn Iterator<Item = ValueAt<T>>>) -> SimpleIteratorStream<'a, T> {
        let peekable = Box::new(it.peekable());
        Self {
            peekable,
            value: T::default(),
            _phantom: std::marker::PhantomData,
        }
    }
}
