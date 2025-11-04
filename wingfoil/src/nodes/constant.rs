use crate::types::*;
use derive_new::new;

/// Only ticks once (on the first [Graph](crate::graph::Graph) cycle).
#[derive(new)]
pub(crate) struct ConstantStream<T: Element> {
    value: T,
}

impl<T: Element> MutableNode for ConstantStream<T> {
    fn cycle(&mut self, _state: &mut GraphState) -> bool {
        true
    }

    fn start(&mut self, state: &mut GraphState) {
        state.add_callback(NanoTime::ZERO);
    }
}

impl<T: Element> StreamPeekRef<T> for ConstantStream<T> {
    fn peek_ref(&self) -> &T {
        &self.value
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
