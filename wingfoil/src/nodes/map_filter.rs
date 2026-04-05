use derive_new::new;

use std::boxed::Box;
use std::rc::Rc;

use crate::types::*;

/// Map's it's source into a new [Stream] using the supplied closure.
/// Used by [map](crate::nodes::StreamOperators::map).
#[derive(new)]
pub struct MapFilterStream<IN, OUT: Element> {
    upstream: Rc<dyn Stream<IN>>,
    #[new(default)]
    value: OUT,
    func: Box<dyn Fn(IN) -> (OUT, bool)>,
}

#[node(active = [upstream], output = value: OUT)]
impl<IN, OUT: Element> MutableNode for MapFilterStream<IN, OUT> {
    fn cycle(&mut self, _state: &mut GraphState) -> anyhow::Result<bool> {
        let (val, ticked) = (self.func)(self.upstream.peek_value());
        if ticked {
            self.value = val;
        }
        Ok(ticked)
    }
}

#[cfg(test)]
mod tests {
    use super::MapFilterStream;
    use crate::graph::*;
    use crate::nodes::*;
    use crate::types::IntoStream;

    #[test]
    fn emits_when_function_returns_true() {
        // Only emit the squared value when the input is odd.
        // count() → 1,2,3,4,5,6; odd inputs: 1,3,5 → squares 1,9,25
        let source = ticker(Duration::from_nanos(100)).count();
        let out =
            MapFilterStream::new(source, Box::new(|x: u64| (x * x, x % 2 == 1))).into_stream();
        let collected = out.collect();
        collected
            .run(RunMode::HistoricalFrom(NanoTime::ZERO), RunFor::Cycles(6))
            .unwrap();
        let values: Vec<u64> = collected.peek_value().iter().map(|v| v.value).collect();
        assert_eq!(values, vec![1, 9, 25]);
    }

    #[test]
    fn suppresses_when_function_returns_false() {
        let source = ticker(Duration::from_nanos(100)).count();
        let out = MapFilterStream::new(source, Box::new(|x: u64| (x, false))).into_stream();
        let collected = out.collect();
        collected
            .run(RunMode::HistoricalFrom(NanoTime::ZERO), RunFor::Cycles(5))
            .unwrap();
        assert!(collected.peek_value().is_empty());
    }
}
