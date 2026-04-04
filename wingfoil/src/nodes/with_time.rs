use derive_new::new;

use std::rc::Rc;

use crate::types::*;

/// Pairs each value with the graph time at which it ticked,
/// producing a `(NanoTime, T)` stream.
/// Used by [with_time](crate::nodes::StreamOperators::with_time).
#[derive(new)]
pub struct WithTimeStream<T: Element> {
    upstream: Rc<dyn Stream<T>>,
    #[new(default)]
    value: (NanoTime, T),
}

#[node(active = [upstream], output = value: (NanoTime, T))]
impl<T: Element> MutableNode for WithTimeStream<T> {
    fn cycle(&mut self, state: &mut GraphState) -> anyhow::Result<bool> {
        self.value = (state.time(), self.upstream.peek_value());
        Ok(true)
    }
}
