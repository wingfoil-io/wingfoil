//! Streaming statistics operators.
//!
//! Two families live here:
//!
//! * **Weighted moments and EWMA** ([MomentStream], [EwmaStream]) — rolled by
//!   hand so they can be *time weighted* (each sample weighted by how long it
//!   was in effect, read from the graph clock) as well as count weighted.  See
//!   [Weighting].
//! * **Rolling moments** ([RollingMomentStream]) — incremental weighted
//!   `mean`/`var`/`std` over a sliding window (count- or time-bounded, under
//!   either weighting), maintained by adding each sample's contribution on
//!   arrival and removing it on eviction, so a tick is O(1) rather than
//!   O(window).
//! * **Windowed order statistics** ([WindowStream]) — a sliding window
//!   recomputed each tick for `min`/`max`/`median` and time-windowed `sum`,
//!   which have no cheap incremental form here.
//! * **watermill adapters** ([StatStream], [WindowStatStream]) — thin wrappers
//!   over the [`watermill`] crate (generic serializable online statistics, a
//!   Rust port of Python `river.stats`) used for the count-based rolling
//!   `sum`/`min`/`max` operators.
//!
//! All operators consume `T: Element + ToPrimitive` and emit `f64`.

use crate::types::*;

use num_traits::ToPrimitive;
use std::collections::VecDeque;
use std::rc::Rc;
use std::time::Duration;
use watermill::maximum::RollingMax;
use watermill::minimum::RollingMin;
use watermill::stats::{RollableUnivariate, Univariate};
use watermill::sum::Sum;

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

/// How an [`ewma`](StatisticsOperators::ewma) decays older observations.
///
/// This is the exponential analogue of [Weighting]: [`PerTick`](EwmaSpan::PerTick)
/// weights by tick count, [`HalfLife`](EwmaSpan::HalfLife) by elapsed time.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum EwmaSpan {
    /// Fixed smoothing factor `alpha` applied once per tick
    /// (`ewma_t = alpha * x_t + (1 - alpha) * ewma_{t-1}`) — count weighting.
    PerTick(f64),
    /// Time decay: a sample's weight halves every `half_life` of elapsed graph
    /// time, independent of tick rate — time weighting.
    HalfLife(Duration),
}

/// The extent of a rolling window: a fixed number of samples, or a fixed span
/// of graph time.
///
/// This is the windowing analogue of [Weighting] and [EwmaSpan] — the
/// count-vs-time choice is an argument to a single operator, not a separate
/// method.  It is orthogonal to [Weighting]: `Window` bounds *which* samples an
/// operator sees, while [Weighting] decides *how* they are weighted, so e.g.
/// `rolling_mean(Window::Count(10), Weighting::Time)` time-weights the last ten
/// samples.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Window {
    /// The most recent `n` samples (clamped to at least one).
    Count(usize),
    /// All samples seen in the last `Duration` of graph time.
    Time(Duration),
}

/// Streaming statistics operators for numeric streams.
///
/// Chain these onto any `Stream<T>` whose values are numeric
/// (`T: Element + ToPrimitive`); every operator emits an `f64`.  Bring the
/// trait into scope with `use wingfoil::adapters::statistics::*` (or
/// `use wingfoil::adapters::statistics::StatisticsOperators`) to use the fluent
/// form:
///
/// ```
/// use wingfoil::*;
/// use wingfoil::adapters::statistics::*;
/// use std::time::Duration;
///
/// let smoothed = ticker(Duration::from_millis(10)).count().ewma(EwmaSpan::PerTick(0.1));
/// let twap = ticker(Duration::from_millis(10)).count().mean(Weighting::Time);
/// ```
pub trait StatisticsOperators<T: Element + ToPrimitive> {
    /// Cumulative mean of source.  [`Weighting::Count`] is the arithmetic
    /// mean of the samples; [`Weighting::Time`] is the time-weighted average,
    /// each value weighted by how long it was in effect.
    #[must_use]
    fn mean(self: &Rc<Self>, weighting: Weighting) -> Rc<dyn Stream<f64>>;
    /// Cumulative variance of source.  [`Weighting::Count`] is the sample
    /// variance (ddof = 1); [`Weighting::Time`] is the time-weighted
    /// (population) variance.  Yields `0.0` until enough data is present.
    #[must_use]
    fn variance(self: &Rc<Self>, weighting: Weighting) -> Rc<dyn Stream<f64>>;
    /// Cumulative standard deviation of source — the square root of
    /// [`variance`](StatisticsOperators::variance) under the same `weighting`.
    #[must_use]
    fn std(self: &Rc<Self>, weighting: Weighting) -> Rc<dyn Stream<f64>>;
    /// Exponentially weighted moving average.  [`EwmaSpan::PerTick`] applies a
    /// fixed smoothing factor once per tick; [`EwmaSpan::HalfLife`] decays by
    /// elapsed time.  The first sample seeds the average.
    #[must_use]
    fn ewma(self: &Rc<Self>, span: EwmaSpan) -> Rc<dyn Stream<f64>>;
    /// Rolling sum over `window` (count- or time-bounded; see [Window]).
    #[must_use]
    fn rolling_sum(self: &Rc<Self>, window: Window) -> Rc<dyn Stream<f64>>;
    /// Rolling mean (simple moving average) over `window`.  [`Weighting::Count`]
    /// is the arithmetic mean; [`Weighting::Time`] weights each retained sample
    /// by how long it was in effect.
    #[must_use]
    fn rolling_mean(self: &Rc<Self>, window: Window, weighting: Weighting) -> Rc<dyn Stream<f64>>;
    /// Rolling minimum over `window`.
    #[must_use]
    fn rolling_min(self: &Rc<Self>, window: Window) -> Rc<dyn Stream<f64>>;
    /// Rolling maximum over `window`.
    #[must_use]
    fn rolling_max(self: &Rc<Self>, window: Window) -> Rc<dyn Stream<f64>>;
    /// Rolling variance over `window`.  [`Weighting::Count`] is the sample
    /// variance (ddof = 1); [`Weighting::Time`] is the time-weighted
    /// (population) variance.  Yields `0.0` until enough data is present.
    #[must_use]
    fn rolling_var(self: &Rc<Self>, window: Window, weighting: Weighting) -> Rc<dyn Stream<f64>>;
    /// Rolling standard deviation over `window` — the square root of
    /// [`rolling_var`](StatisticsOperators::rolling_var) under the same
    /// `weighting`.
    #[must_use]
    fn rolling_std(self: &Rc<Self>, window: Window, weighting: Weighting) -> Rc<dyn Stream<f64>>;
    /// Rolling median over `window`.  [`Weighting::Count`] is the ordinary
    /// median; [`Weighting::Time`] is the time-weighted median (the value at
    /// which cumulative in-effect time crosses one half).
    #[must_use]
    fn rolling_median(self: &Rc<Self>, window: Window, weighting: Weighting)
    -> Rc<dyn Stream<f64>>;
}

impl<T: Element + ToPrimitive + 'static> StatisticsOperators<T> for dyn Stream<T> {
    fn mean(self: &Rc<Self>, weighting: Weighting) -> Rc<dyn Stream<f64>> {
        MomentStream::new(self.clone(), Moment::Mean, weighting).into_stream()
    }

    fn variance(self: &Rc<Self>, weighting: Weighting) -> Rc<dyn Stream<f64>> {
        MomentStream::new(self.clone(), Moment::Var, weighting).into_stream()
    }

    fn std(self: &Rc<Self>, weighting: Weighting) -> Rc<dyn Stream<f64>> {
        MomentStream::new(self.clone(), Moment::Std, weighting).into_stream()
    }

    fn ewma(self: &Rc<Self>, span: EwmaSpan) -> Rc<dyn Stream<f64>> {
        let decay = match span {
            EwmaSpan::PerTick(alpha) => {
                debug_assert!(
                    (0.0..=1.0).contains(&alpha),
                    "ewma PerTick smoothing factor must be in [0, 1], got {alpha}"
                );
                EwmaDecay::PerTick(alpha)
            }
            EwmaSpan::HalfLife(half_life) => EwmaDecay::HalfLife(half_life.as_nanos() as f64),
        };
        EwmaStream::new(self.clone(), decay).into_stream()
    }

    fn rolling_sum(self: &Rc<Self>, window: Window) -> Rc<dyn Stream<f64>> {
        match window {
            // A count window has a fixed size, so watermill's incremental `Sum`
            // (O(1) per tick) beats recomputing over the buffer.
            Window::Count(n) => WindowStatStream::new(self.clone(), Sum::new(), n).into_stream(),
            Window::Time(_) => {
                WindowStream::new(self.clone(), WindowStat::Sum, Weighting::Count, window)
                    .into_stream()
            }
        }
    }

    fn rolling_mean(self: &Rc<Self>, window: Window, weighting: Weighting) -> Rc<dyn Stream<f64>> {
        RollingMomentStream::new(self.clone(), Moment::Mean, weighting, window).into_stream()
    }

    fn rolling_min(self: &Rc<Self>, window: Window) -> Rc<dyn Stream<f64>> {
        match window {
            // watermill's monotonic-deque `RollingMin` is O(1) amortised over a
            // fixed count window; a time window recomputes over the buffer.
            Window::Count(n) => {
                StatStream::new(self.clone(), RollingMin::new(n.max(1))).into_stream()
            }
            Window::Time(_) => {
                WindowStream::new(self.clone(), WindowStat::Min, Weighting::Count, window)
                    .into_stream()
            }
        }
    }

    fn rolling_max(self: &Rc<Self>, window: Window) -> Rc<dyn Stream<f64>> {
        match window {
            Window::Count(n) => {
                StatStream::new(self.clone(), RollingMax::new(n.max(1))).into_stream()
            }
            Window::Time(_) => {
                WindowStream::new(self.clone(), WindowStat::Max, Weighting::Count, window)
                    .into_stream()
            }
        }
    }

    fn rolling_var(self: &Rc<Self>, window: Window, weighting: Weighting) -> Rc<dyn Stream<f64>> {
        RollingMomentStream::new(self.clone(), Moment::Var, weighting, window).into_stream()
    }

    fn rolling_std(self: &Rc<Self>, window: Window, weighting: Weighting) -> Rc<dyn Stream<f64>> {
        RollingMomentStream::new(self.clone(), Moment::Std, weighting, window).into_stream()
    }

    fn rolling_median(
        self: &Rc<Self>,
        window: Window,
        weighting: Weighting,
    ) -> Rc<dyn Stream<f64>> {
        WindowStream::new(self.clone(), WindowStat::Median, weighting, window).into_stream()
    }
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

    /// Exact inverse of [`push`](WeightedMoments::push): drop a `(x, weight)`
    /// previously pushed, so a sliding window can evict its oldest contribution
    /// in O(1).  Like any revert-based scheme this can accumulate floating-point
    /// error over many add/remove cycles, so `m2` is clamped at zero here and
    /// again by [`variance`](WeightedMoments::variance)'s callers.
    fn remove(&mut self, x: f64, weight: f64) {
        if weight <= 0.0 {
            return;
        }
        let w_new = self.w_sum - weight;
        // Removing the final contribution empties the accumulator; reset cleanly
        // rather than dividing by a weight at (or below) zero.
        if self.count <= 1 || w_new <= 0.0 {
            *self = Self::default();
            return;
        }
        self.count -= 1;
        // Recover the pre-push mean, then invert the M2 update with it.
        let mean_old = (self.w_sum * self.mean - weight * x) / w_new;
        self.m2 -= weight * (x - mean_old) * (x - self.mean);
        if self.m2 < 0.0 {
            self.m2 = 0.0;
        }
        self.mean = mean_old;
        self.w_sum = w_new;
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

/// Incremental rolling weighted mean / variance / standard deviation over a
/// sliding [Window].
///
/// Maintains [WeightedMoments] by adding each sample's contribution on arrival
/// and removing it on eviction, so a tick is O(1) rather than recomputing over
/// the window.  A rolling node only cycles when its upstream ticks, so at
/// compute time the newest sample is always "now" — which fixes every sample's
/// weight as soon as its successor arrives and makes both weightings
/// incrementally maintainable:
///
/// * [`Weighting::Count`] — each sample is a unit-weight point, added when it
///   arrives and removed when it leaves the window.
/// * [`Weighting::Time`] — each sample is credited with the interval until its
///   successor.  The newest sample opens an interval that is only committed (and
///   weighted) once the next tick closes it, so it contributes nothing until
///   then — the same left-continuous-step treatment as [MomentStream].
pub(crate) struct RollingMomentStream<T: Element> {
    upstream: Rc<dyn Stream<T>>,
    moment: Moment,
    weighting: Weighting,
    window: Window,
    buffer: VecDeque<(f64, NanoTime)>,
    moments: WeightedMoments,
    value: f64,
}

#[node(active = [upstream], output = value: f64)]
impl<T: Element + ToPrimitive> MutableNode for RollingMomentStream<T> {
    fn cycle(&mut self, state: &mut GraphState) -> anyhow::Result<bool> {
        let now = state.time();
        let sample = self.upstream.peek_value().to_f64().unwrap_or(f64::NAN);

        // Commit the contribution the new sample creates.
        match self.weighting {
            // A count-weighted sample contributes a unit weight immediately.
            Weighting::Count => self.moments.push(sample, 1.0),
            // A time-weighted sample closes the interval opened by the previous
            // sample, crediting it with the elapsed Δt.  The new sample itself
            // contributes nothing until the next tick advances the clock.
            Weighting::Time => {
                if let Some(&(prev_v, prev_t)) = self.buffer.back() {
                    self.moments.push(prev_v, f64::from(now - prev_t));
                }
            }
        }
        self.buffer.push_back((sample, now));

        // Evict from the front, removing each evicted contribution.
        while self.should_evict(now) {
            let (old_v, old_t) = self
                .buffer
                .pop_front()
                .expect("invariant: should_evict implies a front sample");
            match self.weighting {
                Weighting::Count => self.moments.remove(old_v, 1.0),
                // The evicted sample's committed interval ran until the new
                // front's time.  An empty buffer means the sample never had a
                // committed interval (it was the lone/newest sample).
                Weighting::Time => {
                    if let Some(&(_, next_t)) = self.buffer.front() {
                        self.moments.remove(old_v, f64::from(next_t - old_t));
                    }
                }
            }
        }

        self.value = self.output(sample);
        Ok(true)
    }
}

impl<T: Element> RollingMomentStream<T> {
    pub fn new(
        upstream: Rc<dyn Stream<T>>,
        moment: Moment,
        weighting: Weighting,
        window: Window,
    ) -> Self {
        // A window of zero samples is meaningless; clamp to at least one.
        let window = match window {
            Window::Count(n) => Window::Count(n.max(1)),
            Window::Time(_) => window,
        };
        Self {
            upstream,
            moment,
            weighting,
            window,
            buffer: VecDeque::new(),
            moments: WeightedMoments::default(),
            value: f64::NAN,
        }
    }

    fn should_evict(&self, now: NanoTime) -> bool {
        match self.window {
            Window::Count(n) => self.buffer.len() > n,
            Window::Time(duration) => {
                let duration = duration.as_nanos() as u64;
                // Time is monotonic, so `now >= t` and the subtraction never
                // underflows.
                self.buffer
                    .front()
                    .is_some_and(|&(_, t)| u64::from(now) - u64::from(t) > duration)
            }
        }
    }

    fn output(&self, current: f64) -> f64 {
        match self.moment {
            // Before any weight has accumulated the mean seeds to the current
            // sample rather than emitting NaN.
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
/// self-windowing (`RollingMin`, `RollingMax`) or cumulative.
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
/// [`RollableUnivariate`] statistic (used here for `Sum`).
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

/// Which order statistic (or time-windowed sum) a [WindowStream] recomputes
/// over its window.  Rolling `mean`/`var`/`std` use the incremental
/// [RollingMomentStream] instead.
#[derive(Clone, Copy)]
pub(crate) enum WindowStat {
    Sum,
    Min,
    Max,
    Median,
}

/// Rolling `min`/`max`/`median` (and time-windowed `sum`) over a sliding
/// [Window] of the stream — either the last `n` samples ([`Window::Count`]) or
/// the last `duration` of graph time ([`Window::Time`]).
///
/// These are order statistics (plus `sum`) with no cheap incremental form, so
/// the statistic is recomputed over the retained samples each tick — O(window)
/// per tick, which is fine for the window sizes these operators target.  Rolling
/// `mean`/`var`/`std` use the incremental [RollingMomentStream] instead.  Under
/// [`Weighting::Time`] the median weights each sample by the gap until the next
/// sample (the most recent by the gap to `now`); the window's leading edge uses
/// only retained samples, so a value that was in effect *before* the window
/// opened is not carried in.
pub(crate) struct WindowStream<T: Element> {
    upstream: Rc<dyn Stream<T>>,
    stat: WindowStat,
    weighting: Weighting,
    window: Window,
    buffer: VecDeque<(f64, NanoTime)>,
    value: f64,
}

#[node(active = [upstream], output = value: f64)]
impl<T: Element + ToPrimitive> MutableNode for WindowStream<T> {
    fn cycle(&mut self, state: &mut GraphState) -> anyhow::Result<bool> {
        let now = state.time();
        let sample = self.upstream.peek_value().to_f64().unwrap_or(f64::NAN);
        self.buffer.push_back((sample, now));
        match self.window {
            // Keep at most `n` samples (`n >= 1`, clamped at construction).
            Window::Count(n) => {
                while self.buffer.len() > n {
                    self.buffer.pop_front();
                }
            }
            // Evict samples whose age now exceeds the window (time is monotonic,
            // so `now >= t` and the subtraction never underflows).
            Window::Time(duration) => {
                let duration = duration.as_nanos() as u64;
                while let Some(&(_, t)) = self.buffer.front() {
                    if u64::from(now) - u64::from(t) > duration {
                        self.buffer.pop_front();
                    } else {
                        break;
                    }
                }
            }
        }
        self.value = self.compute(now);
        Ok(true)
    }
}

impl<T: Element> WindowStream<T> {
    fn new(
        upstream: Rc<dyn Stream<T>>,
        stat: WindowStat,
        weighting: Weighting,
        window: Window,
    ) -> Self {
        // A window of zero samples is meaningless; clamp to at least one.
        let window = match window {
            Window::Count(n) => Window::Count(n.max(1)),
            Window::Time(_) => window,
        };
        Self {
            upstream,
            stat,
            weighting,
            window,
            buffer: VecDeque::new(),
            value: f64::NAN,
        }
    }

    /// Invoke `f(value, weight)` for each retained sample under the active
    /// weighting: unit weight for [`Weighting::Count`], in-effect Δt (the gap to
    /// the next sample, or to `now` for the most recent) for [`Weighting::Time`].
    fn for_each_weight(&self, now: NanoTime, mut f: impl FnMut(f64, f64)) {
        match self.weighting {
            Weighting::Count => {
                for &(v, _) in &self.buffer {
                    f(v, 1.0);
                }
            }
            Weighting::Time => {
                let n = self.buffer.len();
                for i in 0..n {
                    let (v, t) = self.buffer[i];
                    let next_t = if i + 1 < n { self.buffer[i + 1].1 } else { now };
                    f(v, f64::from(next_t - t));
                }
            }
        }
    }

    /// Median of the retained samples under the active weighting.  Sorts by
    /// value and returns the value at which cumulative weight crosses half the
    /// total; when the crossing lands exactly on a boundary (e.g. an even count
    /// under unit weights) the two straddling values are averaged, so unit
    /// weights reproduce the ordinary median.
    fn weighted_median(&self, now: NanoTime) -> f64 {
        let mut pairs: Vec<(f64, f64)> = Vec::with_capacity(self.buffer.len());
        // Drop zero-weight samples (a time-weighted newest sample with Δt == 0).
        self.for_each_weight(now, |v, w| {
            if w > 0.0 {
                pairs.push((v, w));
            }
        });
        if pairs.is_empty() {
            // Every retained sample had zero weight; fall back to the latest.
            return self.buffer.back().expect("invariant: buffer non-empty").0;
        }
        pairs.sort_by(|a, b| a.0.partial_cmp(&b.0).unwrap_or(std::cmp::Ordering::Equal));
        let half = pairs.iter().map(|&(_, w)| w).sum::<f64>() / 2.0;
        let mut cumulative = 0.0;
        for (i, &(v, w)) in pairs.iter().enumerate() {
            cumulative += w;
            if cumulative > half {
                return v;
            }
            if cumulative == half {
                // Crossing lands exactly on a boundary: average with the next
                // value if there is one (the even-count unit-weight median).
                return match pairs.get(i + 1) {
                    Some(&(next, _)) => (v + next) / 2.0,
                    None => v,
                };
            }
        }
        // Only reachable through floating-point rounding; the last value wins.
        pairs.last().expect("invariant: pairs non-empty").0
    }

    fn compute(&self, now: NanoTime) -> f64 {
        if self.buffer.is_empty() {
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
            WindowStat::Median => self.weighted_median(now),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
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
            .ewma(EwmaSpan::PerTick(0.3));
        ewma.run(RunMode::HistoricalFrom(NanoTime::ZERO), RunFor::Cycles(4))
            .unwrap();
        assert!((ewma.peek_value() - 5.0).abs() < 1e-10);
    }

    #[test]
    fn ewma_of_sequence() {
        // count() gives 1,2,3,4. alpha = 0.5, seeded on the first sample.
        //   e1 = 1; e2 = 1.5; e3 = 2.25; e4 = 3.125
        let ewma = counter().ewma(EwmaSpan::PerTick(0.5));
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
            .ewma(EwmaSpan::PerTick(0.5));
        ewma.run(RunMode::HistoricalFrom(NanoTime::ZERO), RunFor::Cycles(3))
            .unwrap();
        assert!((ewma.peek_value() - 2.5).abs() < 1e-10);
    }

    #[test]
    fn ewma_decay_matches_per_tick_when_dt_equals_half_life() {
        // With Δt (100ns) == half-life, alpha = 1 - 2^-1 = 0.5 every tick, so
        // over 1,2,3,4 the result matches ewma(0.5): 3.125.
        let ewma = counter().ewma(EwmaSpan::HalfLife(Duration::from_nanos(100)));
        ewma.run(RunMode::HistoricalFrom(NanoTime::ZERO), RunFor::Cycles(4))
            .unwrap();
        assert!((ewma.peek_value() - 3.125).abs() < 1e-10);
    }

    #[test]
    fn ewma_decay_constant_stream_is_constant() {
        let ewma = ticker(Duration::from_nanos(100))
            .count()
            .map(|_: u64| 7u64)
            .ewma(EwmaSpan::HalfLife(Duration::from_nanos(250)));
        ewma.run(RunMode::HistoricalFrom(NanoTime::ZERO), RunFor::Cycles(6))
            .unwrap();
        assert!((ewma.peek_value() - 7.0).abs() < 1e-10);
    }

    // ── Weighted mean / variance / std ───────────────────────────────────────

    #[test]
    fn mean_count_is_arithmetic_mean() {
        // 1,2,3,4,5 -> 3.0
        let avg = counter().mean(Weighting::Count);
        avg.run(RunMode::HistoricalFrom(NanoTime::ZERO), RunFor::Cycles(5))
            .unwrap();
        assert!((avg.peek_value() - 3.0).abs() < 1e-10);
    }

    #[test]
    fn mean_time_weighted_lags_by_one_interval() {
        // Ticks are evenly spaced, so each of 1,2,3,4 is in effect for one
        // interval before the next; 5 has not yet been credited.
        // TWA = (1+2+3+4)/4 = 2.5 (vs count mean 3.0).
        let avg = counter().mean(Weighting::Time);
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
        let s = counter().rolling_sum(Window::Count(3));
        s.run(RunMode::HistoricalFrom(NanoTime::ZERO), RunFor::Cycles(5))
            .unwrap();
        assert!((s.peek_value() - 12.0).abs() < 1e-10);
    }

    #[test]
    fn rolling_mean_over_window() {
        // window 3 over 1,2,3,4,5 -> {3,4,5} mean = 4.0
        let s = counter().rolling_mean(Window::Count(3), Weighting::Count);
        s.run(RunMode::HistoricalFrom(NanoTime::ZERO), RunFor::Cycles(5))
            .unwrap();
        assert!((s.peek_value() - 4.0).abs() < 1e-10);
    }

    #[test]
    fn rolling_min_max_over_window() {
        let mn = counter().rolling_min(Window::Count(2));
        let mx = counter().rolling_max(Window::Count(2));
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
        let var = counter().rolling_var(Window::Count(3), Weighting::Count);
        let std = counter().rolling_std(Window::Count(3), Weighting::Count);
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
        // incremental moments can make it a hair negative, so std must clamp at
        // zero rather than emit NaN.
        let std = ticker(Duration::from_nanos(100))
            .count()
            .map(|_: u64| 7u64)
            .rolling_std(Window::Count(3), Weighting::Count);
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
        // Sample variance is defined as 0.0 (not NaN) while n <= ddof.
        let var = counter().rolling_var(Window::Count(3), Weighting::Count);
        var.run(RunMode::HistoricalFrom(NanoTime::ZERO), RunFor::Cycles(1))
            .unwrap();
        assert_eq!(var.peek_value(), 0.0);
    }

    #[test]
    fn rolling_median_over_window() {
        // window 3 over 1,2,3,4,5 -> {3,4,5} median = 4.0
        let med = counter().rolling_median(Window::Count(3), Weighting::Count);
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
        let s = counter().rolling_sum(Window::Time(WIN));
        s.run(RunMode::HistoricalFrom(NanoTime::ZERO), RunFor::Cycles(5))
            .unwrap();
        assert!((s.peek_value() - 12.0).abs() < 1e-10); // {3,4,5}
    }

    #[test]
    fn rolling_mean_over_time_window_count_and_time() {
        // Count: mean{3,4,5} = 4.0
        let mean_c = counter().rolling_mean(Window::Time(WIN), Weighting::Count);
        mean_c
            .run(RunMode::HistoricalFrom(NanoTime::ZERO), RunFor::Cycles(5))
            .unwrap();
        assert!((mean_c.peek_value() - 4.0).abs() < 1e-10);

        // Time: 3 and 4 each held for 100ns, 5 held for 0 (it is "now").
        // TWA = (3*100 + 4*100) / 200 = 3.5
        let mean_t = counter().rolling_mean(Window::Time(WIN), Weighting::Time);
        mean_t
            .run(RunMode::HistoricalFrom(NanoTime::ZERO), RunFor::Cycles(5))
            .unwrap();
        assert!((mean_t.peek_value() - 3.5).abs() < 1e-10);
    }

    #[test]
    fn rolling_min_max_over_time_window() {
        let mn = counter().rolling_min(Window::Time(WIN));
        let mx = counter().rolling_max(Window::Time(WIN));
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
        let var = counter().rolling_var(Window::Time(WIN), Weighting::Count);
        var.run(RunMode::HistoricalFrom(NanoTime::ZERO), RunFor::Cycles(5))
            .unwrap();
        assert!((var.peek_value() - 1.0).abs() < 1e-10);
    }

    #[test]
    fn rolling_var_std_over_time_window_time_weighted() {
        // Time window retains {3@200ns, 4@300ns, 5@400ns}; under time weighting
        // 3 and 4 each held for 100ns and 5 is "now" (Δt = 0, dropped). Committed
        // {3:100, 4:100}: mean 3.5, m2 = 100*0.25 + 100*0.25 = 50, and the
        // time-weighted (population) variance is m2 / w_sum = 50 / 200 = 0.25.
        let var = counter().rolling_var(Window::Time(WIN), Weighting::Time);
        let std = counter().rolling_std(Window::Time(WIN), Weighting::Time);
        var.run(RunMode::HistoricalFrom(NanoTime::ZERO), RunFor::Cycles(5))
            .unwrap();
        std.run(RunMode::HistoricalFrom(NanoTime::ZERO), RunFor::Cycles(5))
            .unwrap();
        assert!((var.peek_value() - 0.25).abs() < 1e-10);
        assert!((std.peek_value() - 0.5).abs() < 1e-10);
    }

    #[test]
    fn rolling_moments_incremental_match_direct_recompute() {
        // Slide a count window across a long, non-trivial sequence and check the
        // incrementally maintained mean/variance still equal a direct recompute
        // of the final window — i.e. add/remove has not drifted.
        const N: u64 = 200;
        const W: usize = 10;
        let seq = |n: u64| ((n % 7) as f64) * 1.5 - 3.0;

        let mean = ticker(Duration::from_nanos(100))
            .count()
            .map(seq)
            .rolling_mean(Window::Count(W), Weighting::Count);
        let var = ticker(Duration::from_nanos(100))
            .count()
            .map(seq)
            .rolling_var(Window::Count(W), Weighting::Count);
        mean.run(
            RunMode::HistoricalFrom(NanoTime::ZERO),
            RunFor::Cycles(N as u32),
        )
        .unwrap();
        var.run(
            RunMode::HistoricalFrom(NanoTime::ZERO),
            RunFor::Cycles(N as u32),
        )
        .unwrap();

        // `count()` emits 1..=N, so the final window is the last W values.
        let window: Vec<f64> = ((N - W as u64 + 1)..=N).map(seq).collect();
        let expected_mean = window.iter().sum::<f64>() / W as f64;
        let expected_var = window
            .iter()
            .map(|v| (v - expected_mean).powi(2))
            .sum::<f64>()
            / (W as f64 - 1.0);

        assert!(
            (mean.peek_value() - expected_mean).abs() < 1e-9,
            "mean {} vs {expected_mean}",
            mean.peek_value()
        );
        assert!(
            (var.peek_value() - expected_var).abs() < 1e-9,
            "var {} vs {expected_var}",
            var.peek_value()
        );
    }

    #[test]
    fn rolling_median_over_time_window() {
        let med = counter().rolling_median(Window::Time(WIN), Weighting::Count);
        med.run(RunMode::HistoricalFrom(NanoTime::ZERO), RunFor::Cycles(5))
            .unwrap();
        assert!((med.peek_value() - 4.0).abs() < 1e-10); // median{3,4,5}
    }

    #[test]
    fn rolling_median_time_weighted() {
        // Count window {2,3,4,5} (last 4 of 1..5). Under time weighting each of
        // 2,3,4 was in effect for one 100ns interval; 5 is "now" (Δt = 0, so it
        // is dropped). Weights {2:100, 3:100, 4:100} — cumulative crosses half
        // (150) at 3, so the weighted median is 3.0 (vs the count median 3.5).
        let med = counter().rolling_median(Window::Count(4), Weighting::Time);
        med.run(RunMode::HistoricalFrom(NanoTime::ZERO), RunFor::Cycles(5))
            .unwrap();
        assert!((med.peek_value() - 3.0).abs() < 1e-10);

        // Same samples, count weighting: median{2,3,4,5} = (3 + 4) / 2 = 3.5.
        let med_c = counter().rolling_median(Window::Count(4), Weighting::Count);
        med_c
            .run(RunMode::HistoricalFrom(NanoTime::ZERO), RunFor::Cycles(5))
            .unwrap();
        assert!((med_c.peek_value() - 3.5).abs() < 1e-10);
    }
}
