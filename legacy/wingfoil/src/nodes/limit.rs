use crate::types::*;
use derive_new::new;
use std::rc::Rc;

#[derive(new)]
pub struct LimitStream<T: Element> {
    source: Rc<dyn Stream<T>>,
    limit: u32,
    #[new(default)]
    tick_count: u32,
    #[new(default)]
    value: T,
}

#[node(active = [source], output = value: T)]
impl<T: Element> MutableNode for LimitStream<T> {
    fn cycle(&mut self, _state: &mut GraphState) -> anyhow::Result<bool> {
        if self.tick_count >= self.limit {
            Ok(false)
        } else {
            self.tick_count += 1;
            self.value = self.source.peek_value();
            Ok(true)
        }
    }
}

#[cfg(test)]
mod tests {
    use crate::graph::*;
    use crate::nodes::*;

    #[test]
    fn suppresses_after_limit_reached() {
        // Source ticks 10 times; limit(3) lets 3 through and suppresses the rest.
        let out = ticker(Duration::from_nanos(100)).count().limit(3).collect();
        out.run(RunMode::HistoricalFrom(NanoTime::ZERO), RunFor::Cycles(10))
            .unwrap();
        assert_eq!(out.peek_value().len(), 3);
    }

    #[test]
    fn emits_correct_values_up_to_limit() {
        let out = ticker(Duration::from_nanos(100)).count().limit(4).collect();
        out.run(RunMode::HistoricalFrom(NanoTime::ZERO), RunFor::Cycles(10))
            .unwrap();
        let values: Vec<u64> = out.peek_value().iter().map(|v| v.value).collect();
        assert_eq!(values, vec![1, 2, 3, 4]);
    }
}
