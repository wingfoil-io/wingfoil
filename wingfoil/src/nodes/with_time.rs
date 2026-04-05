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

#[cfg(test)]
mod tests {
    use crate::graph::*;
    use crate::nodes::*;

    #[test]
    fn timestamps_match_graph_time() {
        // ticker at 100ns intervals starts at ZERO: ticks at 0, 100, 200, 300
        let out = ticker(Duration::from_nanos(100))
            .count()
            .with_time()
            .collect();
        out.run(RunMode::HistoricalFrom(NanoTime::ZERO), RunFor::Cycles(4))
            .unwrap();
        let items = out.peek_value();
        assert_eq!(items.len(), 4);
        // Each emitted (time, value) should have its timestamp equal to the tick time
        for item in &items {
            let (t, _v) = item.value;
            assert_eq!(t, item.time);
        }
        // Spot-check values: count starts at 1
        assert_eq!(items[0].value, (NanoTime::new(0), 1u64));
        assert_eq!(items[3].value, (NanoTime::new(300), 4u64));
    }
}
