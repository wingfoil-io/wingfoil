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
    fn cycle(&mut self, state: &mut GraphState) -> anyhow::Result<bool> {
        if self.tick_count >= self.limit {
            // Request graph termination when limit is reached
            // This allows RunFor::Forever to stop gracefully
            state.request_stop();
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
    fn stops_after_n_ticks_with_run_forever() {
        // RunFor::Forever would run indefinitely without limit; limit(3) must stop it.
        let out = ticker(Duration::from_nanos(100)).count().limit(3).collect();
        out.run(RunMode::HistoricalFrom(NanoTime::ZERO), RunFor::Forever)
            .unwrap();
        assert_eq!(out.peek_value().len(), 3);
    }

    #[test]
    fn emits_correct_values_up_to_limit() {
        let out = ticker(Duration::from_nanos(100)).count().limit(4).collect();
        out.run(RunMode::HistoricalFrom(NanoTime::ZERO), RunFor::Forever)
            .unwrap();
        let values: Vec<u64> = out.peek_value().iter().map(|v| v.value).collect();
        assert_eq!(values, vec![1, 2, 3, 4]);
    }
}
