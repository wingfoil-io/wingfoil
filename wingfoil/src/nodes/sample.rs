use derive_new::new;

use crate::types::*;
use std::rc::Rc;

/// Emit's its source, if and only if, it's trigger ticks.
/// Used by [sample](crate::nodes::StreamOperators::sample).
#[derive(new)]
pub struct SampleStream<T: Element> {
    upstream: Rc<dyn Stream<T>>,
    trigger: Rc<dyn Node>,
    #[new(default)]
    value: T,
}

#[node(passive = [upstream], active = [trigger], output = value: T)]
impl<T: Element> MutableNode for SampleStream<T> {
    fn cycle(&mut self, _state: &mut GraphState) -> anyhow::Result<bool> {
        self.value = self.upstream.peek_value();
        Ok(true)
    }
}

#[cfg(test)]
mod tests {

    use super::*;
    use crate::graph::*;
    use crate::nodes::*;

    #[test]
    fn sample_works() {
        //env_logger::init();
        let c = ConstantStream::new(7).into_stream();
        let ticker1 = ticker(Duration::from_millis(100));
        let ticker2 = ticker(Duration::from_millis(200));
        let node = c
            .sample(ticker1)
            .logged("a", log::Level::Info)
            .sample(ticker2)
            .logged("b", log::Level::Info);
        Graph::new(
            vec![node.as_node()],
            RunMode::HistoricalFrom(NanoTime::ZERO),
            RunFor::Duration(Duration::from_millis(1000)),
        )
        .print()
        .run()
        .unwrap();
    }
}
