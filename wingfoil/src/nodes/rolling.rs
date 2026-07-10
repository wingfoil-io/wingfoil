use crate::types::*;

use num_traits::ToPrimitive;
use std::collections::VecDeque;
use std::rc::Rc;

/// Which statistic a [RollingStream] computes over its window.
#[derive(Clone, Copy)]
pub(crate) enum RollingStat {
    Sum,
    Mean,
    Min,
    Max,
    /// Sample variance (ddof = 1).
    Var,
    /// Sample standard deviation (ddof = 1).
    Std,
}

/// Computes a rolling statistic over the most recent `window` samples.
///
/// Backs the `rolling_*` operators on
/// [StreamOperators](crate::nodes::StreamOperators).  A single node handles
/// every statistic so window eviction lives in one place; the [RollingStat]
/// selects the reduction applied on each tick.
pub(crate) struct RollingStream<T: Element> {
    upstream: Rc<dyn Stream<T>>,
    stat: RollingStat,
    window: usize,
    buffer: VecDeque<f64>,
    value: f64,
}

#[node(active = [upstream], output = value: f64)]
impl<T: Element + ToPrimitive> MutableNode for RollingStream<T> {
    fn cycle(&mut self, _state: &mut GraphState) -> anyhow::Result<bool> {
        let sample = self.upstream.peek_value().to_f64().unwrap_or(f64::NAN);
        self.buffer.push_back(sample);
        if self.buffer.len() > self.window {
            self.buffer.pop_front();
        }
        self.value = self.compute();
        Ok(true)
    }
}

impl<T: Element> RollingStream<T> {
    pub fn new(upstream: Rc<dyn Stream<T>>, stat: RollingStat, window: usize) -> Self {
        // A window of zero is meaningless; clamp to at least one sample.
        let window = window.max(1);
        Self {
            upstream,
            stat,
            window,
            buffer: VecDeque::with_capacity(window),
            value: f64::NAN,
        }
    }

    fn compute(&self) -> f64 {
        let n = self.buffer.len();
        if n == 0 {
            return f64::NAN;
        }
        match self.stat {
            RollingStat::Sum => self.buffer.iter().sum(),
            RollingStat::Mean => self.buffer.iter().sum::<f64>() / n as f64,
            RollingStat::Min => self.buffer.iter().copied().fold(f64::INFINITY, f64::min),
            RollingStat::Max => self
                .buffer
                .iter()
                .copied()
                .fold(f64::NEG_INFINITY, f64::max),
            RollingStat::Var | RollingStat::Std => {
                // Sample variance is undefined for fewer than two points.
                if n < 2 {
                    return f64::NAN;
                }
                let mean = self.buffer.iter().sum::<f64>() / n as f64;
                let sum_sq: f64 = self
                    .buffer
                    .iter()
                    .map(|x| {
                        let d = x - mean;
                        d * d
                    })
                    .sum();
                let var = sum_sq / (n as f64 - 1.0);
                match self.stat {
                    RollingStat::Std => var.sqrt(),
                    _ => var,
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use crate::graph::*;
    use crate::nodes::*;

    /// A stream of 1,2,3,4,5 via `count()`.
    fn counter() -> Rc<dyn Stream<u64>> {
        ticker(Duration::from_nanos(100)).count()
    }

    #[test]
    fn rolling_sum_over_window() {
        // window 3 over 1,2,3,4,5 -> last window is {3,4,5} = 12
        let s = counter().rolling_sum(3);
        s.run(RunMode::HistoricalFrom(NanoTime::ZERO), RunFor::Cycles(5))
            .unwrap();
        assert!((s.peek_value() - 12.0).abs() < 1e-10);
    }

    #[test]
    fn rolling_mean_over_window() {
        // window 3 over 1,2,3,4,5 -> {3,4,5} mean = 4.0
        let s = counter().rolling_mean(3);
        s.run(RunMode::HistoricalFrom(NanoTime::ZERO), RunFor::Cycles(5))
            .unwrap();
        assert!((s.peek_value() - 4.0).abs() < 1e-10);
    }

    #[test]
    fn rolling_min_max_over_window() {
        let mn = counter().rolling_min(2);
        let mx = counter().rolling_max(2);
        mn.run(RunMode::HistoricalFrom(NanoTime::ZERO), RunFor::Cycles(5))
            .unwrap();
        mx.run(RunMode::HistoricalFrom(NanoTime::ZERO), RunFor::Cycles(5))
            .unwrap();
        // last window {4,5}
        assert!((mn.peek_value() - 4.0).abs() < 1e-10);
        assert!((mx.peek_value() - 5.0).abs() < 1e-10);
    }

    #[test]
    fn rolling_var_std_over_window() {
        // window 3 over 1,2,3,4,5 -> {3,4,5}
        // mean = 4, sample var = ((3-4)^2 + (4-4)^2 + (5-4)^2) / (3-1) = 2/2 = 1.0
        let var = counter().rolling_var(3);
        let std = counter().rolling_std(3);
        var.run(RunMode::HistoricalFrom(NanoTime::ZERO), RunFor::Cycles(5))
            .unwrap();
        std.run(RunMode::HistoricalFrom(NanoTime::ZERO), RunFor::Cycles(5))
            .unwrap();
        assert!((var.peek_value() - 1.0).abs() < 1e-10);
        assert!((std.peek_value() - 1.0).abs() < 1e-10);
    }

    #[test]
    fn rolling_var_is_nan_with_single_sample() {
        let var = counter().rolling_var(3);
        var.run(RunMode::HistoricalFrom(NanoTime::ZERO), RunFor::Cycles(1))
            .unwrap();
        assert!(var.peek_value().is_nan());
    }
}
