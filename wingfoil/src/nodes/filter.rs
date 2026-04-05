use derive_new::new;

use std::rc::Rc;

use crate::types::*;

/// Filter's it source based on the supplied predicate.  Used by
/// [filter](crate::nodes::StreamOperators::filter).
#[derive(new)]
pub(crate) struct FilterStream<T: Element> {
    source: Rc<dyn Stream<T>>,
    condition: Rc<dyn Stream<bool>>,
    #[new(default)]
    value: T,
}

#[node(active = [source, condition], output = value: T)]
impl<T: Element> MutableNode for FilterStream<T> {
    fn cycle(&mut self, _state: &mut GraphState) -> anyhow::Result<bool> {
        let val = self.source.peek_value();
        let ticked = self.condition.peek_value();
        if ticked {
            self.value = val;
        }
        Ok(ticked)
    }
}

#[cfg(test)]
mod tests {
    use crate::graph::*;
    use crate::nodes::*;

    #[test]
    fn passes_values_when_condition_true() {
        // count() produces 1,2,3,4,5,6. Condition: value is even.
        // filter_value is the simpler API; filter (condition stream) is the underlying one.
        let filtered = ticker(Duration::from_nanos(100))
            .count()
            .filter_value(|x| x.is_multiple_of(2))
            .collect();
        filtered
            .run(RunMode::HistoricalFrom(NanoTime::ZERO), RunFor::Cycles(6))
            .unwrap();
        let values: Vec<u64> = filtered.peek_value().iter().map(|v| v.value).collect();
        assert_eq!(values, vec![2, 4, 6]);
    }

    #[test]
    fn suppresses_all_when_condition_always_false() {
        let filtered = ticker(Duration::from_nanos(100))
            .count()
            .filter_value(|_| false)
            .collect();
        filtered
            .run(RunMode::HistoricalFrom(NanoTime::ZERO), RunFor::Cycles(5))
            .unwrap();
        assert!(filtered.peek_value().is_empty());
    }

    #[test]
    fn condition_stream_controls_emission() {
        // Source ticks every 100ns. Condition stream is true only on even counts.
        let source = ticker(Duration::from_nanos(100)).count();
        let condition = source.map(|x: u64| x.is_multiple_of(2));
        let filtered = source.filter(condition).collect();
        filtered
            .run(RunMode::HistoricalFrom(NanoTime::ZERO), RunFor::Cycles(6))
            .unwrap();
        // Only even-count ticks pass: values 2, 4, 6
        assert!(filtered.peek_value().iter().all(|v| v.value % 2 == 0));
    }
}
