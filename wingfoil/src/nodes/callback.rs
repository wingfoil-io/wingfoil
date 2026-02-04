use crate::types::*;
use crate::queue::{TimeQueue, ValueAt};
use std::hash::Hash;
use std::fmt::Debug;

/// A [Stream] which can be updated by calling [push](CallBackStream::push).
/// Useful for unit testing.
pub struct CallBackStream<'a, T: Debug + Clone + 'a + Hash + Eq> {
    value: T,
    queue: TimeQueue<T>,
    _phantom: std::marker::PhantomData<&'a T>,
}

impl<'a, T: Debug + Clone + 'a + Hash + Eq + Default> Default for CallBackStream<'a, T> {
    fn default() -> Self {
        Self::new()
    }
}

impl<'a, T: Debug + Clone + 'a + Hash + Eq + Default> CallBackStream<'a, T> {
    pub fn new() -> Self {
        Self {
            value: T::default(),
            queue: TimeQueue::new(),
            _phantom: std::marker::PhantomData,
        }
    }
}

impl<'a, T: Debug + Clone + 'a + Hash + Eq + Default> StreamPeekRef<'a, T> for CallBackStream<'a, T> {
    fn peek_ref(&self) -> &T {
        &self.value
    }
}

impl<'a, T: Debug + Clone + 'a + Hash + Eq + Default> MutableNode<'a> for CallBackStream<'a, T> {
    fn cycle(&mut self, state: &mut GraphState<'a>) -> anyhow::Result<bool> {
        let current_time = state.time();
        let mut ticked = false;
        while self.queue.pending(current_time) {
            self.value = self.queue.pop();
            ticked = true;
        }
        // Schedule next tick if queue is not empty
        if !self.queue.is_empty() {
            state.add_callback(self.queue.next_time());
        }
        Ok(ticked)
    }

    fn upstreams(&self) -> UpStreams<'a> {
        UpStreams::none()
    }

    fn setup(&mut self, state: &mut GraphState<'a>) -> anyhow::Result<()> {
        if !self.queue.is_empty() {
            state.add_callback(self.queue.next_time());
        }
        Ok(())
    }
}

impl<'a, T: Debug + Clone + 'a + Hash + Eq> CallBackStream<'a, T> {
    pub fn push(&mut self, value: ValueAt<T>) {
        self.queue.push(value.value, value.time);
    }
}
