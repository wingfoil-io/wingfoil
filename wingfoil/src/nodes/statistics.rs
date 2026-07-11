//! Streaming statistics operators.
//!
//! Two families live here:
//!
//! * **Weighted moments and EWMA** ([MomentStream], [EwmaStream]) — rolled by
//!   hand so they can be *time weighted* (each sample weighted by how long it
//!   was in effect, read from the graph clock) as well as count weighted.  See
//!   [Weighting].
//! * **watermill adapters** ([StatStream], [WindowStatStream]) — thin wrappers
//!   over the [`watermill`] crate (generic serializable online statistics, a
//!   Rust port of Python `river.stats`) used for the count-based rolling
//!   `sum`/`min`/`max`/`median` operators.
//!
//! All operators consume `T: Element + ToPrimitive` and emit `f64`.

use crate::types::*;

use num_traits::ToPrimitive;
use std::collections::VecDeque;
use std::rc::Rc;
use std::time::Duration;
use watermill::stats::{RollableUnivariate, Univariate};

/// How samples are weighted when aggregating a stream.
///
/// A stream's ticks are rarely evenly spaced, so "the average" is ambiguous:
/// do we average the *samples* or the *signal over time*?  [Weighting] selects
/// between the two.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Weighting {
    /// Every sample counts equally (weight = 1) — the ordinary arithmetic
    /// statistic over the observed values.
    Count,
    /// Each sample is weighted by how long it was in effect: the elapsed time
    /// since the previous sample (weight = Δt in nanoseconds).  This yields
    /// time-weighted averages/variances, where a value that persisted for a
    /// second counts ten times as much as one replaced after 100ms.
    Time,
}

/// Incremental weighted mean and variance via West's algorithm (a weighted
/// generalisation of Welford's — numerically stable, single pass).
#[derive(Default)]
struct WeightedMoments {
    w_sum: f64,
    count: u64,
    mean: f64,
    m2: f64,
}

impl WeightedMoments {
    fn push(&mut self, x: f64, weight: f64) {
        // Zero-duration (simultaneous ticks) or non-positive weights add nothing.
        if weight <= 0.0 {
            return;
        }
        self.w_sum += weight;
        self.count += 1;
        let mean_old = self.mean;
        self.mean += (weight / self.w_sum) * (x - mean_old);
        self.m2 += weight * (x - mean_old) * (x - self.mean);
    }

    fn is_empty(&self) -> bool {
        self.w_sum <= 0.0
    }

    fn mean(&self) -> f64 {
        self.mean
    }

    fn variance(&self, weighting: Weighting) -> f64 {
        match weighting {
            // Sample variance (ddof = 1), matching `rolling_var`.
            Weighting::Count => {
                if self.count < 2 {
                    return 0.0;
                }
                self.m2 / (self.count as f64 - 1.0)
            }
            // Reliability-weighted variance has no clean ddof correction, so we
            // report the population form (divide by the total weight).
            Weighting::Time => {
                if self.w_sum <= 0.0 {
                    return 0.0;
                }
                self.m2 / self.w_sum
            }
        }
    }
}

/// Which moment a [MomentStream] emits.
#[derive(Clone, Copy)]
pub(crate) enum Moment {
    Mean,
    Var,
    Std,
}

/// Cumulative weighted mean / variance / standard deviation over a stream.
///
/// Under [`Weighting::Count`] each sample contributes equally.  Under
/// [`Weighting::Time`] each sample is weighted by the interval it was in
/// effect — so on every tick the *previous* value is credited with the elapsed
/// Δt before the new value takes over.  The most recent sample therefore only
/// starts contributing once the next tick advances the clock, which is the
/// correct treatment of a left-continuous step signal.
pub(crate) struct MomentStream<T: Element> {
    upstream: Rc<dyn Stream<T>>,
    moment: Moment,
    weighting: Weighting,
    moments: WeightedMoments,
    last_time: Option<NanoTime>,
    prev_value: f64,
    value: f64,
}

#[node(active = [upstream], output = value: f64)]
impl<T: Element + ToPrimitive> MutableNode for MomentStream<T> {
    fn cycle(&mut self, state: &mut GraphState) -> anyhow::Result<bool> {
        let sample = self.upstream.peek_value().to_f64().unwrap_or(f64::NAN);
        match self.weighting {
            Weighting::Count => self.moments.push(sample, 1.0),
            Weighting::Time => {
                let now = state.time();
                if let Some(prev_t) = self.last_time {
                    let dt = f64::from(now - prev_t);
                    self.moments.push(self.prev_value, dt);
                }
                self.prev_value = sample;
                self.last_time = Some(now);
            }
        }
        self.value = self.output(sample);
        Ok(true)
    }
}

impl<T: Element> MomentStream<T> {
    pub fn new(upstream: Rc<dyn Stream<T>>, moment: Moment, weighting: Weighting) -> Self {
        Self {
            upstream,
            moment,
            weighting,
            moments: WeightedMoments::default(),
            last_time: None,
            prev_value: f64::NAN,
            value: f64::NAN,
        }
    }

    fn output(&self, current: f64) -> f64 {
        match self.moment {
            // Before any weight has accumulated (time weighting, first tick) the
            // mean seeds to the current sample rather than emitting NaN.
            Moment::Mean if self.moments.is_empty() => current,
            Moment::Mean => self.moments.mean(),
            Moment::Var => self.moments.variance(self.weighting),
            Moment::Std => self.moments.variance(self.weighting).max(0.0).sqrt(),
        }
    }
}

/// How an [EwmaStream] decays older observations.
#[derive(Clone, Copy)]
pub(crate) enum EwmaDecay {
    /// Fixed smoothing factor applied once per tick (count weighting).
    PerTick(f64),
    /// Time decay with the given half-life in nanoseconds: a sample's weight
    /// halves every `half_life` of elapsed time, independent of tick rate.
    HalfLife(f64),
}

/// Exponentially weighted moving average.
///
/// We implement this rather than use `watermill::ewmean::EWMean` for two
/// reasons.  First, watermill (0.1.2) uses `if self.mean == 0.0 { self.mean = x }`
/// as its "not yet initialised" sentinel (see `src/ewmean.rs`), which mis-fires
/// whenever the average legitimately reaches exactly `0.0` — e.g. a
/// difference/returns stream crossing zero — and re-seeds to the next raw
/// sample instead of decaying.  An explicit `initialised` flag avoids that.
/// (No upstream issue tracks this as of watermill 0.1.2.)  Second, owning the
/// node lets us support time-based decay ([`EwmaDecay::HalfLife`]) using the
/// graph clock, which watermill's count-based `EWMean` cannot express.
pub(crate) struct EwmaStream<T: Element> {
    upstream: Rc<dyn Stream<T>>,
    decay: EwmaDecay,
    value: f64,
    initialised: bool,
    last_time: Option<NanoTime>,
}

#[node(active = [upstream], output = value: f64)]
impl<T: Element + ToPrimitive> MutableNode for EwmaStream<T> {
    fn cycle(&mut self, state: &mut GraphState) -> anyhow::Result<bool> {
        let sample = self.upstream.peek_value().to_f64().unwrap_or(f64::NAN);
        if !self.initialised {
            // Seed with the first sample regardless of its value.
            self.value = sample;
            self.initialised = true;
            self.last_time = Some(state.time());
            return Ok(true);
        }
        let alpha = match self.decay {
            EwmaDecay::PerTick(alpha) => alpha,
            EwmaDecay::HalfLife(half_life) => {
                let now = state.time();
                let prev = self
                    .last_time
                    .expect("invariant: last_time set once initialised");
                self.last_time = Some(now);
                if half_life <= 0.0 {
                    // Degenerate half-life: fully adapt to the newest sample.
                    1.0
                } else {
                    // alpha = 1 - 2^(-Δt / half_life)
                    let dt = f64::from(now - prev);
                    1.0 - (-(dt / half_life) * std::f64::consts::LN_2).exp()
                }
            }
        };
        self.value += alpha * (sample - self.value);
        Ok(true)
    }
}

impl<T: Element> EwmaStream<T> {
    pub fn new(upstream: Rc<dyn Stream<T>>, decay: EwmaDecay) -> Self {
        Self {
            upstream,
            decay,
            value: f64::NAN,
            initialised: false,
            last_time: None,
        }
    }
}

/// Feeds each upstream sample into a watermill [`Univariate`] statistic and
/// emits `stat.get()`.  Works for any statistic that owns its own state —
/// self-windowing (`RollingMin`, `RollingMax`, `RollingQuantile`) or
/// cumulative.
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

/// Which statistic a [TimeWindowStream] computes over its time window.
#[derive(Clone, Copy)]
pub(crate) enum WindowStat {
    Sum,
    Mean,
    Min,
    Max,
    Median,
    Var,
    Std,
}

/// Rolling statistic over all samples seen in the last `duration` of graph
/// time (as opposed to the last `n` samples).
///
/// Samples older than `duration` are evicted each tick and the statistic is
/// recomputed over what remains — O(window) per tick, which is fine for the
/// window sizes these operators target.  Under [`Weighting::Time`] each sample
/// is weighted by the gap until the next sample (the most recent by the gap to
/// `now`); the window's leading edge uses only retained samples, so a value
/// that was in effect *before* the window opened is not carried in.
pub(crate) struct TimeWindowStream<T: Element> {
    upstream: Rc<dyn Stream<T>>,
    stat: WindowStat,
    weighting: Weighting,
    duration: u64,
    buffer: VecDeque<(f64, NanoTime)>,
    value: f64,
}

#[node(active = [upstream], output = value: f64)]
impl<T: Element + ToPrimitive> MutableNode for TimeWindowStream<T> {
    fn cycle(&mut self, state: &mut GraphState) -> anyhow::Result<bool> {
        let now = state.time();
        let sample = self.upstream.peek_value().to_f64().unwrap_or(f64::NAN);
        self.buffer.push_back((sample, now));
        // Evict samples whose age now exceeds the window (time is monotonic,
        // so `now >= t` and the subtraction never underflows).
        while let Some(&(_, t)) = self.buffer.front() {
            if u64::from(now) - u64::from(t) > self.duration {
                self.buffer.pop_front();
            } else {
                break;
            }
        }
        self.value = self.compute(now);
        Ok(true)
    }
}

impl<T: Element> TimeWindowStream<T> {
    pub fn new(
        upstream: Rc<dyn Stream<T>>,
        stat: WindowStat,
        weighting: Weighting,
        duration: Duration,
    ) -> Self {
        Self {
            upstream,
            stat,
            weighting,
            duration: duration.as_nanos() as u64,
            buffer: VecDeque::new(),
            value: f64::NAN,
        }
    }

    fn compute(&self, now: NanoTime) -> f64 {
        let n = self.buffer.len();
        if n == 0 {
            return f64::NAN;
        }
        match self.stat {
            WindowStat::Sum => self.buffer.iter().map(|&(v, _)| v).sum(),
            WindowStat::Min => self
                .buffer
                .iter()
                .map(|&(v, _)| v)
                .fold(f64::INFINITY, f64::min),
            WindowStat::Max => self
                .buffer
                .iter()
                .map(|&(v, _)| v)
                .fold(f64::NEG_INFINITY, f64::max),
            WindowStat::Median => {
                let mut vals: Vec<f64> = self.buffer.iter().map(|&(v, _)| v).collect();
                vals.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
                let mid = n / 2;
                if n % 2 == 1 {
                    vals[mid]
                } else {
                    (vals[mid - 1] + vals[mid]) / 2.0
                }
            }
            WindowStat::Mean | WindowStat::Var | WindowStat::Std => {
                let mut moments = WeightedMoments::default();
                match self.weighting {
                    Weighting::Count => {
                        for &(v, _) in &self.buffer {
                            moments.push(v, 1.0);
                        }
                    }
                    Weighting::Time => {
                        for i in 0..n {
                            let (v, t) = self.buffer[i];
                            let next_t = if i + 1 < n { self.buffer[i + 1].1 } else { now };
                            moments.push(v, f64::from(next_t - t));
                        }
                    }
                }
                match self.stat {
                    WindowStat::Mean if moments.is_empty() => {
                        // Only reachable under time weighting when every retained
                        // sample has zero in-effect duration; fall back to latest.
                        self.buffer.back().expect("invariant: buffer non-empty").0
                    }
                    WindowStat::Mean => moments.mean(),
                    WindowStat::Var => moments.variance(self.weighting),
                    WindowStat::Std => moments.variance(self.weighting).max(0.0).sqrt(),
                    _ => unreachable!("outer match limits stat to Mean/Var/Std"),
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use crate::graph::*;
    use crate::nodes::*;

    /// A stream of 1,2,3,4,5 via `count()`, one tick every 100ns.
    fn counter() -> Rc<dyn Stream<u64>> {
        ticker(Duration::from_nanos(100)).count()
    }

    // ── EWMA ─────────────────────────────────────────────────────────────────

    #[test]
    fn ewma_seeds_on_first_sample() {
        // Constant stream of 5 — seeded with the first sample, stays 5.0.
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
        //   e1 = 1; e2 = 1.5; e3 = 2.25; e4 = 3.125
        let ewma = counter().ewma(0.5);
        ewma.run(RunMode::HistoricalFrom(NanoTime::ZERO), RunFor::Cycles(4))
            .unwrap();
        assert!((ewma.peek_value() - 3.125).abs() < 1e-10);
    }

    #[test]
    fn ewma_does_not_reset_at_zero() {
        // Inputs 0, 0, 5 with alpha 0.5. Our EWMA seeds once (to 0) and then
        // decays: 0 -> 0 -> 2.5. watermill's `mean == 0` sentinel would instead
        // re-seed on the 5 and jump to 5.0 — this guards against that.
        let ewma = counter()
            .map(|n: u64| if n <= 2 { 0.0 } else { 5.0 })
            .ewma(0.5);
        ewma.run(RunMode::HistoricalFrom(NanoTime::ZERO), RunFor::Cycles(3))
            .unwrap();
        assert!((ewma.peek_value() - 2.5).abs() < 1e-10);
    }

    #[test]
    fn ewma_decay_matches_per_tick_when_dt_equals_half_life() {
        // With Δt (100ns) == half-life, alpha = 1 - 2^-1 = 0.5 every tick, so
        // over 1,2,3,4 the result matches ewma(0.5): 3.125.
        let ewma = counter().ewma_decay(Duration::from_nanos(100));
        ewma.run(RunMode::HistoricalFrom(NanoTime::ZERO), RunFor::Cycles(4))
            .unwrap();
        assert!((ewma.peek_value() - 3.125).abs() < 1e-10);
    }

    #[test]
    fn ewma_decay_constant_stream_is_constant() {
        let ewma = ticker(Duration::from_nanos(100))
            .count()
            .map(|_: u64| 7u64)
            .ewma_decay(Duration::from_nanos(250));
        ewma.run(RunMode::HistoricalFrom(NanoTime::ZERO), RunFor::Cycles(6))
            .unwrap();
        assert!((ewma.peek_value() - 7.0).abs() < 1e-10);
    }

    // ── Weighted mean / variance / std ───────────────────────────────────────

    #[test]
    fn average_count_is_arithmetic_mean() {
        // 1,2,3,4,5 -> 3.0
        let avg = counter().average(Weighting::Count);
        avg.run(RunMode::HistoricalFrom(NanoTime::ZERO), RunFor::Cycles(5))
            .unwrap();
        assert!((avg.peek_value() - 3.0).abs() < 1e-10);
    }

    #[test]
    fn average_time_weighted_lags_by_one_interval() {
        // Ticks are evenly spaced, so each of 1,2,3,4 is in effect for one
        // interval before the next; 5 has not yet been credited.
        // TWA = (1+2+3+4)/4 = 2.5 (vs count mean 3.0).
        let avg = counter().average(Weighting::Time);
        avg.run(RunMode::HistoricalFrom(NanoTime::ZERO), RunFor::Cycles(5))
            .unwrap();
        assert!((avg.peek_value() - 2.5).abs() < 1e-10);
    }

    #[test]
    fn variance_count_is_sample_variance() {
        // 1,2,3,4,5: mean 3, m2 = 10, sample var = 10 / (5-1) = 2.5
        let var = counter().variance(Weighting::Count);
        let std = counter().std(Weighting::Count);
        var.run(RunMode::HistoricalFrom(NanoTime::ZERO), RunFor::Cycles(5))
            .unwrap();
        std.run(RunMode::HistoricalFrom(NanoTime::ZERO), RunFor::Cycles(5))
            .unwrap();
        assert!((var.peek_value() - 2.5).abs() < 1e-10);
        assert!((std.peek_value() - 2.5_f64.sqrt()).abs() < 1e-10);
    }

    #[test]
    fn variance_time_weighted_is_population_over_weight() {
        // Credited values {1,2,3,4} each weight 100: mean 2.5,
        // m2 = 100 * (2.25+0.25+0.25+2.25) = 500, var = 500 / 400 = 1.25.
        let var = counter().variance(Weighting::Time);
        var.run(RunMode::HistoricalFrom(NanoTime::ZERO), RunFor::Cycles(5))
            .unwrap();
        assert!((var.peek_value() - 1.25).abs() < 1e-10);
    }

    // ── watermill-backed rolling operators ───────────────────────────────────

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
        // window 3 over 1,2,3,4,5 -> {3,4,5}: sample var = 2/2 = 1.0
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
    fn rolling_std_of_constant_window_is_zero_not_nan() {
        // A constant window has zero variance; floating-point cancellation in the
        // revert-based variance can make it a hair negative, so std must clamp at
        // zero rather than emit NaN.
        let std = ticker(Duration::from_nanos(100))
            .count()
            .map(|_: u64| 7u64)
            .rolling_std(3);
        std.run(RunMode::HistoricalFrom(NanoTime::ZERO), RunFor::Cycles(6))
            .unwrap();
        let v = std.peek_value();
        assert!(!v.is_nan(), "rolling_std must not be NaN");
        assert!(
            v.abs() < 1e-10,
            "constant window std should be 0.0, got {v}"
        );
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

    // ── time-windowed operators ──────────────────────────────────────────────
    //
    // Ticks are 100ns apart, so a 250ns window retains the three most recent
    // samples (ages 0, 100, 200) and drops the rest — i.e. {3, 4, 5}.
    const WIN: Duration = Duration::from_nanos(250);

    #[test]
    fn rolling_sum_over_time_window() {
        let s = counter().rolling_sum_over(WIN);
        s.run(RunMode::HistoricalFrom(NanoTime::ZERO), RunFor::Cycles(5))
            .unwrap();
        assert!((s.peek_value() - 12.0).abs() < 1e-10); // {3,4,5}
    }

    #[test]
    fn rolling_mean_over_time_window_count_and_time() {
        // Count: mean{3,4,5} = 4.0
        let mean_c = counter().rolling_mean_over(WIN, Weighting::Count);
        mean_c
            .run(RunMode::HistoricalFrom(NanoTime::ZERO), RunFor::Cycles(5))
            .unwrap();
        assert!((mean_c.peek_value() - 4.0).abs() < 1e-10);

        // Time: 3 and 4 each held for 100ns, 5 held for 0 (it is "now").
        // TWA = (3*100 + 4*100) / 200 = 3.5
        let mean_t = counter().rolling_mean_over(WIN, Weighting::Time);
        mean_t
            .run(RunMode::HistoricalFrom(NanoTime::ZERO), RunFor::Cycles(5))
            .unwrap();
        assert!((mean_t.peek_value() - 3.5).abs() < 1e-10);
    }

    #[test]
    fn rolling_min_max_over_time_window() {
        let mn = counter().rolling_min_over(WIN);
        let mx = counter().rolling_max_over(WIN);
        mn.run(RunMode::HistoricalFrom(NanoTime::ZERO), RunFor::Cycles(5))
            .unwrap();
        mx.run(RunMode::HistoricalFrom(NanoTime::ZERO), RunFor::Cycles(5))
            .unwrap();
        assert!((mn.peek_value() - 3.0).abs() < 1e-10);
        assert!((mx.peek_value() - 5.0).abs() < 1e-10);
    }

    #[test]
    fn rolling_var_over_time_window_count() {
        // Count sample variance of {3,4,5}: mean 4, m2 = 2, var = 2/2 = 1.0
        let var = counter().rolling_var_over(WIN, Weighting::Count);
        var.run(RunMode::HistoricalFrom(NanoTime::ZERO), RunFor::Cycles(5))
            .unwrap();
        assert!((var.peek_value() - 1.0).abs() < 1e-10);
    }

    #[test]
    fn rolling_median_over_time_window() {
        let med = counter().rolling_median_over(WIN);
        med.run(RunMode::HistoricalFrom(NanoTime::ZERO), RunFor::Cycles(5))
            .unwrap();
        assert!((med.peek_value() - 4.0).abs() < 1e-10); // median{3,4,5}
    }
}
