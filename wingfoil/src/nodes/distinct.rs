use crate::types::*;
use derive_new::new;
use std::rc::Rc;

/// Only propagates it's source when it's value changes.  Used
/// by [distinct](crate::nodes::StreamOperators::distinct).
#[derive(new)]
pub(crate) struct DistinctStream<T: Element> {
    source: Rc<dyn Stream<T>>, // the source stream
    #[new(default)] // used by derive_new
    value: T,
}

#[node(active = [source], output = value: T)]
impl<T: Element + PartialEq> MutableNode for DistinctStream<T> {
    // called by Graph when it determines this node needs
    // to be cycled
    fn cycle(&mut self, _state: &mut GraphState) -> anyhow::Result<bool> {
        let curr = self.source.peek_value();
        if self.value == curr {
            // value did not change, do not tick
            Ok(false)
        } else {
            // value changed, tick
            self.value = curr;
            Ok(true)
        }
    }
}

#[cfg(test)]
mod tests {
    use crate::graph::*;
    use crate::nodes::*;

    #[test]
    fn suppresses_repeated_values() {
        // count() → map to (x+1)/2 gives: 1,1,2,2,3,3 for ticks 1..6
        // DistinctStream starts with T::default() = 0; first value (1) differs, so it emits.
        // distinct should emit only when the value changes: 1, 2, 3
        let distinct = ticker(Duration::from_nanos(100))
            .count()
            .map(|x: u64| (x + 1) / 2) // 1,1,2,2,3,3 for ticks 1..6
            .distinct()
            .collect();
        distinct
            .run(RunMode::HistoricalFrom(NanoTime::ZERO), RunFor::Cycles(6))
            .unwrap();
        let values: Vec<u64> = distinct.peek_value().iter().map(|v| v.value).collect();
        assert_eq!(values, vec![1, 2, 3]);
    }

    #[test]
    fn emits_every_tick_when_all_unique() {
        // count() produces 1,2,3,4 — all distinct, so every tick should emit
        let distinct = ticker(Duration::from_nanos(100))
            .count()
            .distinct()
            .collect();
        distinct
            .run(RunMode::HistoricalFrom(NanoTime::ZERO), RunFor::Cycles(4))
            .unwrap();
        assert_eq!(distinct.peek_value().len(), 4);
    }
}
