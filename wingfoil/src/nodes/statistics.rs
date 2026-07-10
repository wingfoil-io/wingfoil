//! Streaming statistics operators backed by the [`watermill`] crate.
//!
//! [`watermill`] provides generic, serializable online statistics (a Rust port
//! of Python `river.stats`).  We adapt its `Univariate` statistics into stream
//! nodes: [StatStream] wraps any self-contained statistic, and
//! [WindowStatStream] adds a fixed-size rolling window around a revertable
//! statistic.  All operators consume `T: Element + ToPrimitive` and emit `f64`,
//! matching the [average](crate::nodes::StreamOperators::average) convention.

use crate::types::*;

use num_traits::ToPrimitive;
use std::collections::VecDeque;
use std::rc::Rc;
use watermill::stats::{RollableUnivariate, Univariate};

/// Feeds each upstream sample into a watermill [`Univariate`] statistic and
/// emits `stat.get()`.  Works for any statistic that owns its own state —
/// cumulative (`Mean`, `Variance`, …), exponentially weighted (`EWMean`,
/// `EWVariance`) or self-windowing (`RollingMin`, `RollingMax`,
/// `RollingQuantile`).
pub(crate) struct StatStream<T: Element, S> {
    upstream: Rc<dyn Stream<T>>,
    stat: S,
    value: f64,
}

#[node(active = [upstream], output = value: f64)]
impl<T: Element + ToPrimitive, S: Univariate<f64> + 'static> MutableNode for StatStream<T, S> {
    fn cycle(&mut self, _state: &mut GraphState) -> anyhow::Result<bool> {
        let sample = self.upstream.peek_value().to_f64().unwrap_or(f64::NAN);
        self.stat.update(sample);
        self.value = self.stat.get();
        Ok(true)
    }
}

impl<T: Element, S> StatStream<T, S> {
    pub fn new(upstream: Rc<dyn Stream<T>>, stat: S) -> Self {
        Self {
            upstream,
            stat,
            value: f64::NAN,
        }
    }
}

/// Rolling window of the most recent `window` samples over a watermill
/// [`RollableUnivariate`] statistic (`Sum`, `Mean`, `Variance`).
///
/// watermill's own `Rolling` wrapper borrows the statistic `&mut`, which cannot
/// be embedded in a long-lived node without self-referential lifetimes, so we
/// own both the statistic and the window here and replicate its
/// evict-then-update logic: when the window is full the oldest sample is
/// reverted out before the newest is folded in.
pub(crate) struct WindowStatStream<T: Element, R> {
    upstream: Rc<dyn Stream<T>>,
    stat: R,
    window: usize,
    buffer: VecDeque<f64>,
    value: f64,
}

#[node(active = [upstream], output = value: f64)]
impl<T: Element + ToPrimitive, R: RollableUnivariate<f64> + 'static> MutableNode
    for WindowStatStream<T, R>
{
    fn cycle(&mut self, _state: &mut GraphState) -> anyhow::Result<bool> {
        let sample = self.upstream.peek_value().to_f64().unwrap_or(f64::NAN);
        if self.buffer.len() == self.window {
            // The window is full and its size is >= 1, so the buffer is non-empty.
            let oldest = self
                .buffer
                .pop_front()
                .expect("invariant: full window is non-empty");
            self.stat
                .revert(oldest)
                .map_err(|e| anyhow::anyhow!("rolling statistic revert failed: {e}"))?;
        }
        self.buffer.push_back(sample);
        self.stat.update(sample);
        self.value = self.stat.get();
        Ok(true)
    }
}

impl<T: Element, R> WindowStatStream<T, R> {
    pub fn new(upstream: Rc<dyn Stream<T>>, stat: R, window: usize) -> Self {
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
    fn ewma_seeds_on_first_sample() {
        // Constant stream of 5 — watermill's EWMean seeds with the first
        // sample, so a constant stream stays at 5.0.
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
        // count() gives 1,2,3,4. alpha = 0.5, seeded on the first sample.
        //   e1 = 1
        //   e2 = 0.5*2 + 0.5*1    = 1.5
        //   e3 = 0.5*3 + 0.5*1.5  = 2.25
        //   e4 = 0.5*4 + 0.5*2.25 = 3.125
        let ewma = counter().ewma(0.5);
        ewma.run(RunMode::HistoricalFrom(NanoTime::ZERO), RunFor::Cycles(4))
            .unwrap();
        assert!((ewma.peek_value() - 3.125).abs() < 1e-10);
    }

    #[test]
    fn rolling_sum_over_window() {
        // window 3 over 1,2,3,4,5 -> last window {3,4,5} = 12
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
    fn rolling_var_is_zero_with_single_sample() {
        // watermill's Variance returns 0.0 (not NaN) while n <= ddof.
        let var = counter().rolling_var(3);
        var.run(RunMode::HistoricalFrom(NanoTime::ZERO), RunFor::Cycles(1))
            .unwrap();
        assert_eq!(var.peek_value(), 0.0);
    }

    #[test]
    fn rolling_median_over_window() {
        // window 3 over 1,2,3,4,5 -> {3,4,5} median = 4.0
        let med = counter().rolling_median(3);
        med.run(RunMode::HistoricalFrom(NanoTime::ZERO), RunFor::Cycles(5))
            .unwrap();
        assert!((med.peek_value() - 4.0).abs() < 1e-10);
    }
}
