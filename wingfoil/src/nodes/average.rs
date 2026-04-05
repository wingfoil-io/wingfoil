use crate::types::*;

use derive_new::new;

use num_traits::ToPrimitive;
use std::rc::Rc;

/// Computes running average.  Used by [average](crate::nodes::StreamOperators::average).
#[derive(new)]
pub(crate) struct AverageStream<T: Element> {
    upstream: Rc<dyn Stream<T>>,
    #[new(default)]
    value: f64,
    #[new(default)]
    count: u64,
}

#[node(active = [upstream], output = value: f64)]
impl<T: Element + ToPrimitive> MutableNode for AverageStream<T> {
    fn cycle(&mut self, _state: &mut GraphState) -> anyhow::Result<bool> {
        self.count += 1;
        let sample = self.upstream.peek_value().to_f64().unwrap_or(f64::NAN);
        self.value += (sample - self.value) / self.count as f64;
        Ok(true)
    }
}

#[cfg(test)]
mod tests {
    use crate::graph::*;
    use crate::nodes::*;

    #[test]
    fn average_of_sequence() {
        // count() gives 1,2,3,4,5. Mean = 3.0
        let avg = ticker(Duration::from_nanos(100)).count().average();
        avg.run(RunMode::HistoricalFrom(NanoTime::ZERO), RunFor::Cycles(5))
            .unwrap();
        assert!((avg.peek_value() - 3.0).abs() < 1e-10);
    }

    #[test]
    fn average_of_constant_stream() {
        // Constant value 7 — running average should always be 7.0
        let avg = ticker(Duration::from_nanos(100))
            .count()
            .map(|_: u64| 7u64)
            .average();
        avg.run(RunMode::HistoricalFrom(NanoTime::ZERO), RunFor::Cycles(10))
            .unwrap();
        assert!((avg.peek_value() - 7.0).abs() < 1e-10);
    }
}
