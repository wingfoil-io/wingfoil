use crate::types::*;
use derive_new::new;

/// Only ticks once (on the first [Graph](crate::graph::Graph) cycle).
#[derive(new)]
pub struct ConstantStream<T: Element> {
    value: T,
}

#[node(output = value: T)]
impl<T: Element> MutableNode for ConstantStream<T> {
    fn cycle(&mut self, _state: &mut GraphState) -> anyhow::Result<bool> {
        Ok(self.cycle_inline())
    }

    fn start(&mut self, state: &mut GraphState) -> anyhow::Result<()> {
        state.add_callback(state.start_time());
        Ok(())
    }
}

impl<T: Element> ConstantStream<T> {
    /// The node's cycle logic, single-sourced: `MutableNode::cycle` delegates
    /// here. Keeping it in a standalone method lets callers invoke it directly
    /// for static dispatch without the `GraphState`/`Result` plumbing.
    #[doc(hidden)]
    pub fn cycle_inline(&mut self) -> bool {
        true
    }
}

#[cfg(test)]
mod tests {

    use crate::*;

    #[test]
    fn constant_value_works() {
        let x = 7;
        let const_value = constant(x);
        assert_eq!(const_value.peek_value(), x);
        let captured = const_value.collect();
        captured
            .run(RunMode::HistoricalFrom(NanoTime::ZERO), RunFor::Forever)
            .unwrap();
        let expected = vec![ValueAt {
            value: x,
            time: NanoTime::new(0),
        }];
        assert_eq!(expected, captured.peek_value());
    }
}
