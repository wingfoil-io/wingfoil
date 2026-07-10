use crate::types::*;

use num_traits::ToPrimitive;
use std::rc::Rc;

/// Computes an exponentially weighted moving average.  Used by
/// [ewma](crate::nodes::StreamOperators::ewma).
pub(crate) struct EwmaStream<T: Element> {
    upstream: Rc<dyn Stream<T>>,
    alpha: f64,
    value: f64,
    initialised: bool,
}

#[node(active = [upstream], output = value: f64)]
impl<T: Element + ToPrimitive> MutableNode for EwmaStream<T> {
    fn cycle(&mut self, _state: &mut GraphState) -> anyhow::Result<bool> {
        let sample = self.upstream.peek_value().to_f64().unwrap_or(f64::NAN);
        if self.initialised {
            self.value = self.alpha * sample + (1.0 - self.alpha) * self.value;
        } else {
            // Seed the average with the first sample.
            self.value = sample;
            self.initialised = true;
        }
        Ok(true)
    }
}

impl<T: Element> EwmaStream<T> {
    pub fn new(upstream: Rc<dyn Stream<T>>, alpha: f64) -> Self {
        Self {
            upstream,
            alpha,
            value: f64::NAN,
            initialised: false,
        }
    }
}

#[cfg(test)]
mod tests {
    use crate::graph::*;
    use crate::nodes::*;

    #[test]
    fn ewma_seeds_on_first_sample() {
        // Constant stream of 5 — first tick seeds to 5.0, stays 5.0 thereafter.
        let ewma = ticker(Duration::from_nanos(100))
            .count()
            .map(|_: u64| 5u64)
            .ewma(0.3);
        ewma.run(RunMode::HistoricalFrom(NanoTime::ZERO), RunFor::Cycles(4))
            .unwrap();
        assert!((ewma.peek_value() - 5.0).abs() < 1e-10);
    }

    #[test]
    fn ewma_of_sequence() {
        // count() gives 1,2,3,4. alpha = 0.5.
        //   e1 = 1
        //   e2 = 0.5*2 + 0.5*1   = 1.5
        //   e3 = 0.5*3 + 0.5*1.5 = 2.25
        //   e4 = 0.5*4 + 0.5*2.25 = 3.125
        let ewma = ticker(Duration::from_nanos(100)).count().ewma(0.5);
        ewma.run(RunMode::HistoricalFrom(NanoTime::ZERO), RunFor::Cycles(4))
            .unwrap();
        assert!((ewma.peek_value() - 3.125).abs() < 1e-10);
    }
}
