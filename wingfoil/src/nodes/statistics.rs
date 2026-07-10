//! Streaming rolling-statistics operators, backing the `ewma` / `rolling_*`
//! methods on [`StreamOperators`](crate::nodes::StreamOperators).
//!
//! These sit alongside [`average`](crate::nodes::StreamOperators::average)
//! (cumulative) and [`window`](crate::nodes::StreamOperators::window)
//! (time-bucketed). Each statistic is provided by
//! [`watermill`](https://crates.io/crates/watermill), a generic online-statistics
//! crate (a Rust port of Python `river.stats`). Two small adapter nodes bridge
//! watermill's `Univariate` / `Revertable` traits into wingfoil stream nodes:
//!
//! - [`StatStream`] feeds each sample into a self-contained watermill statistic
//!   (`EWMean`, `RollingMin`, `RollingMax`, `RollingQuantile`) that owns its own
//!   windowing, and emits `get()`.
//! - [`WindowStatStream`] owns a `VecDeque` window around a *revertable*
//!   statistic (`Sum`, `Mean`, `Variance`), replicating watermill's `Rolling`
//!   evict-then-update logic. (watermill's own `Rolling` borrows the statistic
//!   `&mut`, so it cannot be embedded directly in a long-lived node.)
//!
//! All operators accept any `T: Element + ToPrimitive` and emit `f64`, matching
//! the [`average`](crate::nodes::StreamOperators::average) convention.

use crate::types::*;

use derive_new::new;

use num_traits::ToPrimitive;
use std::collections::VecDeque;
use std::rc::Rc;

use watermill::ewmean::EWMean;
use watermill::maximum::RollingMax;
use watermill::mean::Mean;
use watermill::minimum::RollingMin;
use watermill::quantile::RollingQuantile;
use watermill::stats::{RollableUnivariate, Univariate};
use watermill::sum::Sum;
use watermill::variance::Variance;

/// Feeds each sample into a self-contained watermill [`Univariate`] statistic
/// and emits its current `get()` value. Used for statistics that manage their
/// own window internally (`EWMean`, `RollingMin`, `RollingMax`,
/// `RollingQuantile`).
#[derive(new)]
pub(crate) struct StatStream<T: Element, S> {
    upstream: Rc<dyn Stream<T>>,
    stat: S,
    #[new(default)]
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

/// Maintains a rolling window of the last `window_size` samples around a
/// *revertable* watermill statistic, emitting `get()` each tick. Used for
/// statistics that watermill only exposes as a running (unbounded) form plus a
/// `revert`: `Sum`, `Mean`, `Variance`. On each sample the oldest value is
/// reverted out once the window is full, then the new value is added — the same
/// evict-then-update logic as watermill's `Rolling` wrapper.
#[derive(new)]
pub(crate) struct WindowStatStream<T: Element, S> {
    upstream: Rc<dyn Stream<T>>,
    stat: S,
    window_size: usize,
    #[new(default)]
    window: VecDeque<f64>,
    #[new(default)]
    value: f64,
}

#[node(active = [upstream], output = value: f64)]
impl<T: Element + ToPrimitive, S: RollableUnivariate<f64> + 'static> MutableNode
    for WindowStatStream<T, S>
{
    fn cycle(&mut self, _state: &mut GraphState) -> anyhow::Result<bool> {
        let sample = self.upstream.peek_value().to_f64().unwrap_or(f64::NAN);
        if self.window.len() == self.window_size
            && let Some(oldest) = self.window.pop_front()
        {
            self.stat
                .revert(oldest)
                .map_err(|e| anyhow::anyhow!("rolling statistic revert failed: {e}"))?;
        }
        self.window.push_back(sample);
        self.stat.update(sample);
        self.value = self.stat.get();
        Ok(true)
    }
}

// Builder functions that keep the concrete `watermill` statistic types
// encapsulated in this module; `StreamOperators` just calls these.

/// Exponentially weighted moving average (`watermill::EWMean`), seeded with the
/// first sample. Backs [`ewma`](crate::nodes::StreamOperators::ewma).
pub(crate) fn ewma_stream<T: Element + ToPrimitive>(
    upstream: Rc<dyn Stream<T>>,
    alpha: f64,
) -> Rc<dyn Stream<f64>> {
    StatStream::new(upstream, EWMean::new(alpha)).into_stream()
}

/// Rolling sum over the last `n` samples.
pub(crate) fn rolling_sum_stream<T: Element + ToPrimitive>(
    upstream: Rc<dyn Stream<T>>,
    n: usize,
) -> Rc<dyn Stream<f64>> {
    WindowStatStream::new(upstream, Sum::new(), n).into_stream()
}

/// Rolling (simple moving) mean over the last `n` samples.
pub(crate) fn rolling_mean_stream<T: Element + ToPrimitive>(
    upstream: Rc<dyn Stream<T>>,
    n: usize,
) -> Rc<dyn Stream<f64>> {
    WindowStatStream::new(upstream, Mean::new(), n).into_stream()
}

/// Rolling sample variance (`ddof = 1`) over the last `n` samples. Emits `0.0`
/// until at least two samples are present.
pub(crate) fn rolling_var_stream<T: Element + ToPrimitive>(
    upstream: Rc<dyn Stream<T>>,
    n: usize,
) -> Rc<dyn Stream<f64>> {
    WindowStatStream::new(upstream, Variance::new(1), n).into_stream()
}

/// Rolling minimum over the last `n` samples.
pub(crate) fn rolling_min_stream<T: Element + ToPrimitive>(
    upstream: Rc<dyn Stream<T>>,
    n: usize,
) -> Rc<dyn Stream<f64>> {
    StatStream::new(upstream, RollingMin::new(n)).into_stream()
}

/// Rolling maximum over the last `n` samples.
pub(crate) fn rolling_max_stream<T: Element + ToPrimitive>(
    upstream: Rc<dyn Stream<T>>,
    n: usize,
) -> Rc<dyn Stream<f64>> {
    StatStream::new(upstream, RollingMax::new(n)).into_stream()
}

/// Rolling median (50th percentile, linearly interpolated) over the last `n`
/// samples.
pub(crate) fn rolling_median_stream<T: Element + ToPrimitive>(
    upstream: Rc<dyn Stream<T>>,
    n: usize,
) -> Rc<dyn Stream<f64>> {
    // `RollingQuantile::new` only rejects a quantile outside [0, 1]; 0.5 is
    // always valid, so this cannot fail.
    let quantile =
        RollingQuantile::new(0.5, n).expect("invariant: 0.5 is a valid quantile in [0, 1]");
    StatStream::new(upstream, quantile).into_stream()
}

#[cfg(test)]
mod tests {
    use crate::graph::*;
    use crate::nodes::*;

    /// Drive a stream of the supplied samples through `build`, running one cycle
    /// per sample, and return the final emitted value.
    fn run_stat<F>(samples: &[f64], build: F) -> f64
    where
        F: FnOnce(Rc<dyn Stream<f64>>) -> Rc<dyn Stream<f64>>,
    {
        let data: Vec<f64> = samples.to_vec();
        let index = std::cell::Cell::new(0usize);
        let source = ticker(Duration::from_nanos(100))
            .count()
            .map(move |_: u64| {
                let i = index.get();
                index.set(i + 1);
                data[i]
            });
        let out = build(source);
        out.run(
            RunMode::HistoricalFrom(NanoTime::ZERO),
            RunFor::Cycles(samples.len() as u32),
        )
        .unwrap();
        out.peek_value()
    }

    fn approx(a: f64, b: f64) {
        assert!((a - b).abs() < 1e-9, "expected {b}, got {a}");
    }

    #[test]
    fn rolling_sum_last_n() {
        // window of 3 over 1..=5 → last three are 3,4,5 → 12
        approx(
            run_stat(&[1.0, 2.0, 3.0, 4.0, 5.0], |s| s.rolling_sum(3)),
            12.0,
        );
    }

    #[test]
    fn rolling_mean_is_simple_moving_average() {
        // last three of 1..=5 are 3,4,5 → mean 4.0
        approx(
            run_stat(&[1.0, 2.0, 3.0, 4.0, 5.0], |s| s.rolling_mean(3)),
            4.0,
        );
    }

    #[test]
    fn rolling_min_and_max_track_window() {
        // window 3 over [5,3,8,1,9,2] → last three 1,9,2 → min 1, max 9
        let samples = [5.0, 3.0, 8.0, 1.0, 9.0, 2.0];
        approx(run_stat(&samples, |s| s.rolling_min(3)), 1.0);
        approx(run_stat(&samples, |s| s.rolling_max(3)), 9.0);
    }

    #[test]
    fn rolling_var_and_std_sample_ddof1() {
        // last three of 1..=5 are 3,4,5. Sample variance (ddof=1) = 1.0, std = 1.0
        let samples = [1.0, 2.0, 3.0, 4.0, 5.0];
        approx(run_stat(&samples, |s| s.rolling_var(3)), 1.0);
        approx(run_stat(&samples, |s| s.rolling_std(3)), 1.0);
    }

    #[test]
    fn rolling_var_is_zero_until_two_samples() {
        // A single sample cannot have a sample variance (n <= ddof) → 0.0, not NaN.
        approx(run_stat(&[42.0], |s| s.rolling_var(5)), 0.0);
    }

    #[test]
    fn rolling_median_of_window() {
        // window 5 over [1,2,3,4,5] → median 3.0
        approx(
            run_stat(&[1.0, 2.0, 3.0, 4.0, 5.0], |s| s.rolling_median(5)),
            3.0,
        );
    }

    #[test]
    fn ewma_seeds_with_first_sample() {
        // EWMean seeds with the first sample, then blends: v = v + alpha*(x - v).
        // samples 10, 20 with alpha 0.5 → seed 10, then 10 + 0.5*(20-10) = 15.
        approx(run_stat(&[10.0, 20.0], |s| s.ewma(0.5)), 15.0);
    }

    #[test]
    fn ewma_of_constant_is_constant() {
        approx(run_stat(&[7.0, 7.0, 7.0, 7.0], |s| s.ewma(0.3)), 7.0);
    }
}
