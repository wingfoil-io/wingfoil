//! Adpaters to convert an Iterators into a Streams

use crate::queue::ValueAt;
use crate::types::*;

use std::cmp::Ordering;

type Peeker<T> = Box<std::iter::Peekable<Box<dyn Iterator<Item = ValueAt<T>>>>>;

/// Wraps an Iterator and exposes it as a stream of Vectors
pub struct IteratorStream<T: Element> {
    peekable: Peeker<T>,
    value: Vec<T>,
}

fn add_callback<T>(peekable: &mut Peeker<T>, state: &mut GraphState) -> bool {
    match peekable.peek() {
        Some(value_at) => {
            state.add_callback(value_at.time);
            true
        }
        None => false,
    }
}

impl<T: Element> MutableNode for IteratorStream<T> {
    fn cycle(&mut self, state: &mut GraphState) -> bool {
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

    fn start(&mut self, state: &mut GraphState) {
        add_callback(&mut self.peekable, state);
    }
}

impl<T: Element> StreamPeekRef<Vec<T>> for IteratorStream<T> {
    fn peek_ref(&self) -> &Vec<T> {
        &self.value
    }
}

impl<T> IteratorStream<T>
where
    T: Element + 'static,
{
    pub fn new(it: Box<dyn Iterator<Item = ValueAt<T>>>) -> Self {
        let peekable = Box::new(it.peekable());
        Self {
            peekable,
            value: vec![],
        }
    }
}

/// Wraps an Iterator and exposes it as a [Stream] of values.
/// The source must be strictly ascending in time.   If the source
/// can tick multiple times at one time, then you can use an
/// [IteratorStream] instead, which emits `Vec<T>`.
pub struct SimpleIteratorStream<T: Element> {
    peekable: Peeker<T>,
    value: T,
}

impl<T: Element> MutableNode for SimpleIteratorStream<T> {
    fn cycle(&mut self, state: &mut GraphState) -> bool {
        {
            let val_at1 = self.peekable.next().unwrap();
            self.value = val_at1.value;

            let optional_val_at2 = self.peekable.peek();
            if let Some(val_at2) = optional_val_at2 {
                match val_at1.time.cmp(&val_at2.time) {
                    Ordering::Greater => {
                        panic!("source time was descending!");
                    }
                    Ordering::Equal => {
                        panic!("source produced multiple ticks for same time, use IteratorStream instead")
                    }
                    Ordering::Less => {}
                }
            }
        }
        add_callback(&mut self.peekable, state)
    }

    fn start(&mut self, state: &mut GraphState) {
        add_callback(&mut self.peekable, state);
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
        let peekable = Box::new(it.peekable());
        Self {
            peekable,
            value: T::default(),
        }
    }
}
