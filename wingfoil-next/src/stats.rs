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

    /// Cumulative sum over every value seen so far (an unbounded / expanding
    /// window) — a running total, O(1) per tick.
    fn cumulative_sum(&self) -> Stream<f64>;

    /// Cumulative arithmetic mean over every value seen so far — O(1) per tick
    /// via Welford's online moments.
    fn cumulative_mean(&self) -> Stream<f64>;

    /// Cumulative minimum over every value seen so far — a running extreme.
    fn cumulative_min(&self) -> Stream<f64>;

    /// Cumulative maximum over every value seen so far — a running extreme.
    fn cumulative_max(&self) -> Stream<f64>;

    /// Cumulative **sample** variance (ddof = 1) over every value seen so far —
    /// the classic statistics adapter's count-weighted convention (divisor
    /// `n - 1`, `0.0` until two values are present), maintained incrementally
    /// with Welford's online moments.
    fn cumulative_var(&self) -> Stream<f64>;

    /// Cumulative **sample** standard deviation over every value seen so far —
    /// the square root of [`cumulative_var`](Self::cumulative_var) under the
    /// same (ddof = 1) convention.
    fn cumulative_std(&self) -> Stream<f64>;

    /// Cumulative median over every value seen so far (an even count averages
    /// its two middle values). Retains all samples, so its memory grows with
    /// the stream.
    fn cumulative_median(&self) -> Stream<f64>;
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

    fn cumulative_sum(&self) -> Stream<f64> {
        self.wire(|b, h| b.cumulative_sum(h))
    }

    fn cumulative_mean(&self) -> Stream<f64> {
        self.wire(|b, h| b.cumulative_mean(h))
    }

    fn cumulative_min(&self) -> Stream<f64> {
        self.wire(|b, h| b.cumulative_min(h))
    }

    fn cumulative_max(&self) -> Stream<f64> {
        self.wire(|b, h| b.cumulative_max(h))
    }

    fn cumulative_var(&self) -> Stream<f64> {
        self.wire(|b, h| b.cumulative_var(h))
    }

    fn cumulative_std(&self) -> Stream<f64> {
        self.wire(|b, h| b.cumulative_std(h))
    }

    fn cumulative_median(&self) -> Stream<f64> {
        self.wire(|b, h| b.cumulative_median(h))
    }
}
