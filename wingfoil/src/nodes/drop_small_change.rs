use crate::types::*;
use derive_new::new;
use std::rc::Rc;

/// Suppresses source ticks while the change from the last emitted value is
/// considered small. Used by
/// [`drop_small_change`](crate::nodes::StreamOperators::drop_small_change).
#[derive(new)]
pub(crate) struct DropSmallChangeStream<T: Element> {
    source: Rc<dyn Stream<T>>,
    is_small: Box<dyn Fn(&T, &T) -> bool>,
    #[new(default)]
    value: T,
    #[new(default)]
    last_emitted: Option<T>,
}

#[node(active = [source], output = value: T)]
impl<T: Element> MutableNode for DropSmallChangeStream<T> {
    fn cycle(&mut self, _state: &mut GraphState) -> anyhow::Result<bool> {
        let current = self.source.peek_value();
        let should_emit = match &self.last_emitted {
            None => true,
            Some(previous) => !(self.is_small)(&current, previous),
        };

        if should_emit {
            self.last_emitted = Some(current.clone());
            self.value = current;
        }

        Ok(should_emit)
    }
}

#[cfg(test)]
mod tests {
    use crate::graph::*;
    use crate::nodes::*;

    #[test]
    fn first_tick_always_propagates() {
        let collected = ticker(Duration::from_nanos(100))
            .count()
            .drop_small_change(|_, _| true)
            .collect();

        collected
            .run(RunMode::HistoricalFrom(NanoTime::ZERO), RunFor::Cycles(3))
            .unwrap();

        let values: Vec<u64> = collected.peek_value().iter().map(|v| v.value).collect();
        assert_eq!(values, vec![1]);
    }

    #[test]
    fn compares_f64_changes_to_last_emitted_value() {
        let collected = ticker(Duration::from_nanos(100))
            .count()
            .map(|count| match count {
                1 => 100.000_f64,
                2 => 100.005,
                3 => 100.020,
                _ => 100.025,
            })
            .drop_small_change(|current, previous| (current - previous).abs() < 0.01)
            .collect();

        collected
            .run(RunMode::HistoricalFrom(NanoTime::ZERO), RunFor::Cycles(4))
            .unwrap();

        let values: Vec<f64> = collected.peek_value().iter().map(|v| v.value).collect();
        assert_eq!(values, vec![100.000, 100.020]);
    }

    #[test]
    fn propagates_every_tick_when_predicate_returns_false() {
        let collected = ticker(Duration::from_nanos(100))
            .count()
            .drop_small_change(|_, _| false)
            .collect();

        collected
            .run(RunMode::HistoricalFrom(NanoTime::ZERO), RunFor::Cycles(4))
            .unwrap();

        let values: Vec<u64> = collected.peek_value().iter().map(|v| v.value).collect();
        assert_eq!(values, vec![1, 2, 3, 4]);
    }
}
