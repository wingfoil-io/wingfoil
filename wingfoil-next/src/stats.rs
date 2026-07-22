//! Statistics combinators — the EWMA and rolling-window ops, as a *separate*
//! extension trait ([`StatisticsOps`]) on `Stream<f64>`.
//!
//! Kept out of the [`prelude`](crate::prelude) so the core combinator
//! vocabulary stays small: bring these in explicitly with
//! `use wingfoil_next::stats::StatisticsOps;` when you need them. This is the
//! pattern any adapter-specific op set follows — an independent trait layered
//! over the [`Stream::wire`](crate::fluent::Stream::wire) extension primitive.

use std::time::Duration;

use crate::fluent::Stream;
use crate::ops::EwmaDecay;

/// Exponentially-weighted moving average and rolling-window statistics over an
/// `f64` stream. An extension trait, so `use`ing it enables
/// `stream.ewma_per_tick(0.5)` / `stream.rolling_mean(3)` chaining.
pub trait StatisticsOps {
    /// Exponentially-weighted moving average with an explicit
    /// [`EwmaDecay`] policy (per-tick alpha or clock half-life).
    fn ewma(&self, decay: EwmaDecay) -> Stream<f64>;

    /// EWMA with a fixed smoothing factor `alpha` applied once per tick,
    /// seeded on the first sample.
    fn ewma_per_tick(&self, alpha: f64) -> Stream<f64>;

    /// EWMA whose weights decay off engine time: a sample's weight halves
    /// every `half_life` of elapsed time, independent of tick rate.
    fn ewma_half_life(&self, half_life: Duration) -> Stream<f64>;

    /// Sum over a sliding window of the last `window` values.
    fn rolling_sum(&self, window: usize) -> Stream<f64>;

    /// Mean over a sliding window of the last `window` values.
    fn rolling_mean(&self, window: usize) -> Stream<f64>;

    /// Minimum over a sliding window of the last `window` values.
    fn rolling_min(&self, window: usize) -> Stream<f64>;

    /// Maximum over a sliding window of the last `window` values.
    fn rolling_max(&self, window: usize) -> Stream<f64>;

    /// Sample variance (ddof = 1) over a sliding window of the last `window`
    /// values — the classic statistics adapter's count-weighted convention
    /// (divisor `n - 1`, `0.0` until two samples are present).
    fn rolling_var(&self, window: usize) -> Stream<f64>;

    /// Sample standard deviation over a sliding window of the last `window`
    /// values — the square root of [`rolling_var`](Self::rolling_var) under the
    /// same (ddof = 1) convention.
    fn rolling_std(&self, window: usize) -> Stream<f64>;

    /// Median over a sliding window of the last `window` values (an even window
    /// averages its two middle values).
    fn rolling_median(&self, window: usize) -> Stream<f64>;

    /// Cumulative **time-weighted** mean over every value seen so far: each
    /// sample is weighted by the interval it was in effect (Δt read from the
    /// graph clock), so a value that persisted longer counts proportionally
    /// more. The most recent sample is credited only once the next tick
    /// advances the clock; the first sample seeds the mean.
    fn time_weighted_mean(&self) -> Stream<f64>;

    /// Cumulative **time-weighted** (population) variance over every value seen
    /// so far — `m2 / Σ Δt`, maintained incrementally with West's
    /// weighted-Welford moments. `0.0` until weight has accumulated.
    fn time_weighted_var(&self) -> Stream<f64>;

    /// Cumulative **time-weighted** standard deviation over every value seen so
    /// far — the square root of [`time_weighted_var`](Self::time_weighted_var).
    fn time_weighted_std(&self) -> Stream<f64>;
}

impl StatisticsOps for Stream<f64> {
    fn ewma(&self, decay: EwmaDecay) -> Stream<f64> {
        self.wire(|b, h| b.ewma(h, decay))
    }

    fn ewma_per_tick(&self, alpha: f64) -> Stream<f64> {
        debug_assert!(
            (0.0..=1.0).contains(&alpha),
            "ewma PerTick smoothing factor must be in [0, 1], got {alpha}"
        );
        self.wire(|b, h| b.ewma_per_tick(h, alpha))
    }

    fn ewma_half_life(&self, half_life: Duration) -> Stream<f64> {
        self.wire(|b, h| b.ewma_half_life(h, half_life))
    }

    fn rolling_sum(&self, window: usize) -> Stream<f64> {
        self.wire(|b, h| b.rolling_sum(h, window))
    }

    fn rolling_mean(&self, window: usize) -> Stream<f64> {
        self.wire(|b, h| b.rolling_mean(h, window))
    }

    fn rolling_min(&self, window: usize) -> Stream<f64> {
        self.wire(|b, h| b.rolling_min(h, window))
    }

    fn rolling_max(&self, window: usize) -> Stream<f64> {
        self.wire(|b, h| b.rolling_max(h, window))
    }

    fn rolling_var(&self, window: usize) -> Stream<f64> {
        self.wire(|b, h| b.rolling_var(h, window))
    }

    fn rolling_std(&self, window: usize) -> Stream<f64> {
        self.wire(|b, h| b.rolling_std(h, window))
    }

    fn rolling_median(&self, window: usize) -> Stream<f64> {
        self.wire(|b, h| b.rolling_median(h, window))
    }

    fn time_weighted_mean(&self) -> Stream<f64> {
        self.wire(|b, h| b.time_weighted_mean(h))
    }

    fn time_weighted_var(&self) -> Stream<f64> {
        self.wire(|b, h| b.time_weighted_var(h))
    }

    fn time_weighted_std(&self) -> Stream<f64> {
        self.wire(|b, h| b.time_weighted_std(h))
    }
}
