use std::cmp::Eq;
use std::hash::Hash;

use crate::queue::{TimeQueue, ValueAt};
use crate::types::*;

use derive_new::new;

/// A queue of values that are emitted at specified times.  Useful for
/// unit tests.  Can also be used to feed stream output back into
/// the [Graph](crate::graph::Graph) as input on later cycles.
#[derive(new)]
pub struct CallBackStream<T: Element + Hash + Eq> {
    #[new(default)]
    value: T,
    #[new(default)]
    queue: TimeQueue<T>,
}

impl<T: Element + Hash + Eq> StreamPeekRef<T> for CallBackStream<T> {
    fn peek_ref(&self) -> &T {
        &self.value
    }
}

impl<T: Element + Hash + Eq> MutableNode for CallBackStream<T> {
    fn cycle(&mut self, state: &mut GraphState) -> bool {
        let mut ticked = false;
        while self.queue.pending(state.time()) {
            self.value = self.queue.pop();
            ticked = true;
        }
        if !self.queue.is_empty() {
            let callback_time = self.queue.next_time();
            state.add_callback(callback_time);
        }
        ticked
    }

    fn start(&mut self, state: &mut GraphState) {
        if !self.queue.is_empty() {
            let time = self.queue.next_time();
            state.add_callback(time);
        }
    }
}

impl<T: Element + Hash + Eq> CallBackStream<T> {
    pub fn push(&mut self, value_at: ValueAt<T>) {
        println!("push {:?}", value_at.time);
        self.queue.push(value_at.value, value_at.time)
    }
}
