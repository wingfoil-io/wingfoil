use derive_new::new;

use std::ops::Sub;
use std::rc::Rc;

use crate::types::*;

/// Emits the difference of it's source from one cycle to the
/// next.  Used by [difference](crate::nodes::StreamOperators::difference).
#[derive(new)]
pub(crate) struct DifferenceStream<T: Element> {
    upstream: Rc<dyn Stream<T>>,
    #[new(default)]
    diff: T,
    #[new(default)]
    prev_val: Option<T>,
}

#[node(active = [upstream], output = diff: T)]
impl<T: Element + Sub<Output = T>> MutableNode for DifferenceStream<T> {
    fn cycle(&mut self, _state: &mut GraphState) -> anyhow::Result<bool> {
        let ticked = match self.prev_val.clone() {
            Some(prv) => {
                self.diff = self.upstream.peek_value() - prv;
                true
            }
            None => false,
        };
        self.prev_val = Some(self.upstream.peek_value());
        Ok(ticked)
    }
}

#[cfg(test)]
mod tests {
    use crate::graph::*;
    use crate::nodes::*;

    #[test]
    fn first_tick_does_not_emit() {
        // count() emits 1 on the first cycle; difference needs a previous value
        // to compute a delta, so it must not emit on the first tick.
        let diff = ticker(Duration::from_nanos(100))
            .count()
            .difference()
            .collect();
        diff.run(RunMode::HistoricalFrom(NanoTime::ZERO), RunFor::Cycles(4))
            .unwrap();
        // 4 source ticks → 3 difference ticks (first skipped)
        assert_eq!(diff.peek_value().len(), 3);
    }

    #[test]
    fn delta_is_correct() {
        // count() produces 1, 2, 3, 4 — all consecutive differences are 1.
        let diff = ticker(Duration::from_nanos(100))
            .count()
            .difference()
            .collect();
        diff.run(RunMode::HistoricalFrom(NanoTime::ZERO), RunFor::Cycles(5))
            .unwrap();
        for v in &diff.peek_value() {
            assert_eq!(v.value, 1u64);
        }
    }

    #[test]
    fn delta_for_non_unit_steps() {
        // map count to squares: 1, 4, 9, 16 → differences 3, 5, 7
        let diff = ticker(Duration::from_nanos(100))
            .count()
            .map(|x: u64| x * x)
            .difference()
            .collect();
        diff.run(RunMode::HistoricalFrom(NanoTime::ZERO), RunFor::Cycles(4))
            .unwrap();
        let values: Vec<u64> = diff.peek_value().iter().map(|v| v.value).collect();
        assert_eq!(values, vec![3, 5, 7]);
    }
}
