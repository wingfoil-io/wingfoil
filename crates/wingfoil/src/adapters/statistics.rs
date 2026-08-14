//! Statistics combinators — the EWMA and rolling-window ops, as a *separate*
//! extension trait ([`StatisticsOps`]) on `Stream<f64>`.
//!
//! Kept out of the [`prelude`](crate::prelude) so the core combinator
//! vocabulary stays small: bring these in explicitly with
//! `use wingfoil::adapters::statistics::StatisticsOps;` when you need them —
//! an independent trait layered over the
//! [`Stream::wire`](crate::fluent::Stream::wire) extension primitive, which is
//! the shape every adapter's op set takes.
//!
//! # Why this is an adapter
//!
//! It is the same path legacy shipped it at (`wingfoil::adapters::statistics`),
//! and it belongs with its neighbours rather than in the core: an opt-in trait,
//! out of the prelude, behind a feature. Being pure computation does not
//! separate it from [`augurs`](crate::adapters::augurs) (a compute library, no
//! service), [`cache`](crate::adapters::cache) (a utility with no graph edge)
//! or [`market`](crate::adapters::market) (a vocabulary that connects to
//! nothing) — "adapter" in this tree means *optional, gated, opt-in*, not
//! *I/O*.
//!
//! # The feature gates the surface, not a build cost
//!
//! Nothing here is behind an external crate — the statistics are hand-rolled,
//! as legacy's were. The ops themselves live in the always-compiled
//! [`ops`](crate::ops) catalog, so `statistics` controls only whether this
//! fluent trait exists. Legacy shipped it ungated; the gate is what makes it
//! consistent with every other adapter.
//!
//! One consequence, and it is the only user-visible one: `nitro!` does not
//! glob feature-gated traits into its generated modules, so a statistics op
//! inside a `nitro!` block needs `use wingfoil::adapters::statistics::StatisticsOps;`
//! in the surrounding file — the same import it needs outside one.

use std::time::Duration;

use crate::fluent::Stream;
// See `fluent.rs`: the generated methods call the ops' generated `Builder`
// methods, which arrive on per-op extension traits.
use crate::ops::*;

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
    /// values — the legacy statistics adapter's count-weighted convention
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
    /// the legacy statistics adapter's count-weighted convention (divisor
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

    /// Sum over a bounded **time** window — the samples seen in the last
    /// `window` of graph time (an entry exactly `window` old is retained). O(1)
    /// per tick.
    fn time_windowed_sum(&self, window: Duration) -> Stream<f64>;

    /// Arithmetic mean over a bounded time window (count-weighted — the ordinary
    /// mean of the samples in the window). O(1) amortised per tick.
    fn time_windowed_mean(&self, window: Duration) -> Stream<f64>;

    /// Minimum over a bounded time window, via a monotonic deque — O(1)
    /// amortised per tick.
    fn time_windowed_min(&self, window: Duration) -> Stream<f64>;

    /// Maximum over a bounded time window, via a monotonic deque — O(1)
    /// amortised per tick.
    fn time_windowed_max(&self, window: Duration) -> Stream<f64>;

    /// **Sample** variance (ddof = 1) over a bounded time window — `0.0` until
    /// two samples are in the window. O(1) amortised per tick.
    fn time_windowed_var(&self, window: Duration) -> Stream<f64>;

    /// **Sample** standard deviation over a bounded time window — the square
    /// root of [`time_windowed_var`](Self::time_windowed_var).
    fn time_windowed_std(&self, window: Duration) -> Stream<f64>;

    /// Median over a bounded time window (an even count averages its two middle
    /// values). Recomputed per tick over the retained window.
    fn time_windowed_median(&self, window: Duration) -> Stream<f64>;

    // ── time-weighted moments (Weighting::Time) ──────────────────────────────
    //
    // The time-*weighted* twin of the moment ops above: each sample is weighted
    // by how long it was in effect (the Δt until its successor, read from engine
    // time) rather than by a unit count, so a value that persisted twice as long
    // counts twice. Ported from the legacy `MomentStream` / `RollingMomentStream`
    // under `Weighting::Time`. The most recent sample only starts contributing
    // once the next tick advances the clock (left-continuous step signal), the
    // mean seeds to the current sample until any weight has accumulated, and the
    // variance is the time-weighted **population** form (`m2 / w_sum`, no ddof
    // correction), `0.0` until weight is present (`std` clamps at zero).

    /// Cumulative time-weighted mean over every sample seen so far (an unbounded
    /// window) — each sample weighted by how long it was in effect.
    fn cumulative_mean_time_weighted(&self) -> Stream<f64>;

    /// Cumulative time-weighted **population** variance over every sample seen so
    /// far — `m2 / w_sum`, `0.0` until weight is present.
    fn cumulative_var_time_weighted(&self) -> Stream<f64>;

    /// Cumulative time-weighted standard deviation over every sample seen so far
    /// — the square root of [`cumulative_var_time_weighted`](Self::cumulative_var_time_weighted).
    fn cumulative_std_time_weighted(&self) -> Stream<f64>;

    /// Time-weighted mean over the most recent `window` samples (a count window).
    fn rolling_mean_time_weighted(&self, window: usize) -> Stream<f64>;

    /// Time-weighted **population** variance over the most recent `window`
    /// samples (a count window).
    fn rolling_var_time_weighted(&self, window: usize) -> Stream<f64>;

    /// Time-weighted standard deviation over the most recent `window` samples (a
    /// count window) — the square root of
    /// [`rolling_var_time_weighted`](Self::rolling_var_time_weighted).
    fn rolling_std_time_weighted(&self, window: usize) -> Stream<f64>;

    /// Time-weighted mean over a bounded time window — the samples seen in the
    /// last `window` of graph time, each weighted by how long it was in effect.
    fn time_windowed_mean_time_weighted(&self, window: Duration) -> Stream<f64>;

    /// Time-weighted **population** variance over a bounded time window.
    fn time_windowed_var_time_weighted(&self, window: Duration) -> Stream<f64>;

    /// Time-weighted standard deviation over a bounded time window — the square
    /// root of
    /// [`time_windowed_var_time_weighted`](Self::time_windowed_var_time_weighted).
    fn time_windowed_std_time_weighted(&self, window: Duration) -> Stream<f64>;

    // ── time-weighted median (Weighting::Time) ───────────────────────────────
    //
    // The time-weighted twin of the median ops above: each retained sample is
    // weighted by how long it was in effect (the Δt until its successor, read
    // from engine time) and the median is the value at which cumulative weight
    // crosses half the total. The newest sample carries zero weight until the
    // next tick advances the clock, so it does not shift the median on arrival.
    // Recompute-per-tick (the median has no cheap incremental form), ported from
    // the legacy `WindowStream::weighted_median` under `Weighting::Time`.

    /// Cumulative time-weighted median over every sample seen so far (an
    /// unbounded window) — each sample weighted by how long it was in effect.
    /// Retains all samples, so its memory grows with the stream.
    fn cumulative_median_time_weighted(&self) -> Stream<f64>;

    /// Time-weighted median over the most recent `window` samples (a count
    /// window), each weighted by how long it was in effect.
    fn rolling_median_time_weighted(&self, window: usize) -> Stream<f64>;

    /// Time-weighted median over a bounded time window — the samples seen in the
    /// last `window` of graph time, each weighted by how long it was in effect.
    fn time_windowed_median_time_weighted(&self, window: Duration) -> Stream<f64>;
}

impl StatisticsOps for Stream<f64> {
    __wf_fluent_ewma!();

    fn ewma_per_tick(&self, alpha: f64) -> Stream<f64> {
        debug_assert!(
            (0.0..=1.0).contains(&alpha),
            "ewma PerTick smoothing factor must be in [0, 1], got {alpha}"
        );
        self.wire(|b, h| b.ewma_per_tick(h, alpha))
    }

    __wf_fluent_ewma_half_life!();

    __wf_fluent_rolling_sum!();

    __wf_fluent_rolling_mean!();

    __wf_fluent_rolling_min!();

    __wf_fluent_rolling_max!();

    __wf_fluent_rolling_var!();

    __wf_fluent_rolling_std!();

    __wf_fluent_rolling_median!();

    __wf_fluent_cumulative_sum!();

    __wf_fluent_cumulative_mean!();

    __wf_fluent_cumulative_min!();

    __wf_fluent_cumulative_max!();

    __wf_fluent_cumulative_var!();

    __wf_fluent_cumulative_std!();

    __wf_fluent_cumulative_median!();

    __wf_fluent_time_windowed_sum!();

    __wf_fluent_time_windowed_mean!();

    __wf_fluent_time_windowed_min!();

    __wf_fluent_time_windowed_max!();

    __wf_fluent_time_windowed_var!();

    __wf_fluent_time_windowed_std!();

    __wf_fluent_time_windowed_median!();

    __wf_fluent_cumulative_mean_time_weighted!();

    __wf_fluent_cumulative_var_time_weighted!();

    __wf_fluent_cumulative_std_time_weighted!();

    __wf_fluent_rolling_mean_time_weighted!();

    __wf_fluent_rolling_var_time_weighted!();

    __wf_fluent_rolling_std_time_weighted!();

    __wf_fluent_time_windowed_mean_time_weighted!();

    __wf_fluent_time_windowed_var_time_weighted!();

    __wf_fluent_time_windowed_std_time_weighted!();

    __wf_fluent_cumulative_median_time_weighted!();

    __wf_fluent_rolling_median_time_weighted!();

    __wf_fluent_time_windowed_median_time_weighted!();
}
