//! The shared latency data layer: latency records, stage markers, the
//! [`Traced`] payload wrapper and the per-stage statistics.
//!
//! Engine-agnostic by construction — it is plain data plus the
//! [`latency_stages!`] generator, with no reference to either engine's node or
//! op machinery. Both trees use it: this crate's `latency` module builds its
//! stamping ops on top, and the legacy `wingfoil` crate re-exports these
//! items and builds its `StampStream` nodes on the same types, so a `Traced`
//! payload crosses the engine boundary unchanged.
//!
//! See [`runtime`](super) for why the shared core lives on the wingfoil side.

use std::marker::PhantomData;

/// Declarative macro that generates a `#[repr(C)]` named-field latency record
/// plus per-stage marker types. See the [module docs](self) for usage.
pub use wingfoil_derive::latency_stages;

// ---------------------------------------------------------------------------
// Core traits
// ---------------------------------------------------------------------------

/// A fixed-size, named-field collection of `u64` nanosecond timestamps.
///
/// Implementors must be `#[repr(C)]` packed `u64` fields (or strictly
/// equivalent), so that the in-memory layout matches `[u64; N]` and the
/// generated `stamps`/`stamp_mut` slice views are sound.
///
/// Use the [`latency_stages!`] macro to generate an implementation; rolling
/// your own is supported but you must uphold the layout invariant.
pub trait Latency: Copy + ::std::fmt::Debug + Default + 'static {
    /// Number of stages.
    const N: usize;
    /// Stage names, in stamp order.
    fn stage_names() -> &'static [&'static str];
    /// Borrow the raw `[u64; N]` view of the stamps.
    fn stamps(&self) -> &[u64];
    /// Borrow a single stamp mutably by index. Panics if `idx >= N`.
    fn stamp_mut(&mut self, idx: usize) -> &mut u64;
}

/// A compile-time marker identifying one stage within a [`Latency`] record.
///
/// The [`latency_stages!`] macro emits one zero-sized `Stage` impl per field.
pub trait Stage<L: Latency> {
    /// Stage name (matches the field identifier).
    const NAME: &'static str;
    /// Index into `L::stamps()`.
    const INDEX: usize;

    /// Write `t` (nanos) into the stage's slot.
    #[inline]
    fn stamp(latency: &mut L, t: u64) {
        *latency.stamp_mut(Self::INDEX) = t;
    }
}

/// A payload that carries an embedded [`Latency`] record.
///
/// Implemented automatically for [`Traced<T, L>`]. Hand-roll if you embed a
/// latency record as a sub-field of a richer payload.
pub trait HasLatency {
    type L: Latency;
    fn latency(&self) -> &Self::L;
    fn latency_mut(&mut self) -> &mut Self::L;
}

// ---------------------------------------------------------------------------
// Traced wrapper
// ---------------------------------------------------------------------------

/// A payload `T` paired with a latency record `L`.
///
/// `#[repr(C)]` with `payload` first so that, for typical payloads (alignment
/// ≤ 8) and `L: Latency` (all `u64` fields, alignment 8), no padding is
/// inserted between the two.
#[repr(C)]
#[derive(
    Clone, Copy, Debug, Default, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize,
)]
pub struct Traced<T, L> {
    pub payload: T,
    pub latency: L,
}

impl<T, L> Traced<T, L> {
    /// Construct a `Traced<T, L>` from a payload, defaulting the latency record.
    #[inline]
    pub fn new(payload: T) -> Self
    where
        L: Default,
    {
        Self {
            payload,
            latency: L::default(),
        }
    }

    /// Construct from explicit payload + latency.
    #[inline]
    pub fn with_latency(payload: T, latency: L) -> Self {
        Self { payload, latency }
    }
}

impl<T, L: Latency> HasLatency for Traced<T, L> {
    type L = L;
    #[inline]
    fn latency(&self) -> &L {
        &self.latency
    }
    #[inline]
    fn latency_mut(&mut self) -> &mut L {
        &mut self.latency
    }
}

// SAFETY: `Traced<T, L>` is `#[repr(C)]` with two fields. When both `T` and
// `L` are themselves `ZeroCopySend`, the composite is self-contained and has
// a uniform memory representation, satisfying the trait's invariants.
//
// The default `ZeroCopySend::type_name()` returns `core::any::type_name::<Self>()`,
// which embeds the absolute Rust paths of `T` and `L`. When the same struct is
// declared via `#[path = "..."] mod ...;` from two different binary crates (a
// common pattern for sharing an iceoryx2 payload between two example binaries),
// the paths differ and iceoryx2 reports `IncompatibleTypes` even though the
// memory layouts are identical. We compose the name from `T::type_name()` and
// `L::type_name()` so leaf overrides via `#[type_name(...)]` propagate up.
#[cfg(feature = "iceoryx2")]
unsafe impl<T, L> iceoryx2::prelude::ZeroCopySend for Traced<T, L>
where
    T: iceoryx2::prelude::ZeroCopySend,
    L: iceoryx2::prelude::ZeroCopySend,
{
    unsafe fn type_name() -> &'static str {
        traced_type_name(unsafe { T::type_name() }, unsafe { L::type_name() })
    }
}

#[cfg(feature = "iceoryx2")]
fn traced_type_name(t: &'static str, l: &'static str) -> &'static str {
    use std::collections::HashMap;
    use std::sync::{Mutex, OnceLock};
    static CACHE: OnceLock<Mutex<HashMap<(&'static str, &'static str), &'static str>>> =
        OnceLock::new();
    let cache = CACHE.get_or_init(|| Mutex::new(HashMap::new()));
    let mut guard = cache.lock().expect("traced type-name cache mutex poisoned");
    if let Some(s) = guard.get(&(t, l)) {
        return s;
    }
    let composed: &'static str = Box::leak(format!("wingfoil::Traced<{t}, {l}>").into_boxed_str());
    guard.insert((t, l), composed);
    composed
}

// ---------------------------------------------------------------------------
// Histogram layout
// ---------------------------------------------------------------------------
//
// An HDR-style histogram: each power-of-two octave is subdivided into
// `SUB_BUCKET_COUNT` equal-width buckets, which bounds the *relative* width of
// any bucket — and therefore the relative error of any quantile read out of it —
// at `1 / SUB_BUCKET_COUNT`, independent of magnitude.
//
// This replaces a plain log2 histogram whose buckets were one octave wide. That
// layout carried up to 100% relative error and `quantile_ns` returned the
// bucket's *upper* bound, so a reported p99 could exceed the observed maximum —
// which it visibly did in the `showcase/latency` report (p99 262144 ns = 2^18
// against a max of 164493 ns). For a library whose subject is latency, a
// percentile that cannot distinguish 70 µs from 131 µs is not usable for
// capacity or SLA work.

/// Bits of sub-bucket resolution within each octave: `2^5 = 32` divisions, so no
/// reported quantile carries more than `1/32` = **3.125%** relative error.
const SUB_BUCKET_BITS: u32 = 5;

/// Divisions per octave — also the size of the exact, single-nanosecond region
/// at the bottom of the range (`[0, 32)`), where a bucket is one value wide and
/// the quantile is therefore exact.
const SUB_BUCKET_COUNT: usize = 1 << SUB_BUCKET_BITS;

/// Octaves above this saturate into the top bucket. `2^34` ns ≈ 17.2 s — beyond
/// any per-hop latency worth a percentile, and `max_ns` still records the true
/// value of an outlier that lands there.
const MAX_OCTAVE: u32 = 34;

/// Number of buckets in a [`StageStats`] histogram: the exact `[0, 32)` region
/// plus [`SUB_BUCKET_COUNT`] divisions for each octave from `2^5` to
/// `2^MAX_OCTAVE`.
///
/// Public because [`StageStats::histogram`] is, so a downstream aggregator can
/// size a matching array.
pub const HISTOGRAM_BUCKETS: usize =
    SUB_BUCKET_COUNT + (MAX_OCTAVE - SUB_BUCKET_BITS) as usize * SUB_BUCKET_COUNT;

/// The bucket a delta of `ns` records into.
///
/// Contiguous and monotonic in `ns`: bucket `i`'s range ends exactly where
/// bucket `i + 1`'s begins (see [`bucket_bounds`]), so no value falls between
/// two buckets and a larger value never lands in a lower bucket.
#[inline]
const fn bucket_index(ns: u64) -> usize {
    if ns < SUB_BUCKET_COUNT as u64 {
        // Exact region: one bucket per nanosecond.
        return ns as usize;
    }
    let octave = ns.ilog2();
    if octave >= MAX_OCTAVE {
        return HISTOGRAM_BUCKETS - 1;
    }
    // Position within the octave, in units of the octave's bucket width.
    let part = ((ns - (1 << octave)) >> (octave - SUB_BUCKET_BITS)) as usize;
    SUB_BUCKET_COUNT + (octave - SUB_BUCKET_BITS) as usize * SUB_BUCKET_COUNT + part
}

/// The half-open value range `[lo, hi)` that bucket `i` covers.
///
/// The top bucket is saturating — it also absorbs everything at or above
/// `2^MAX_OCTAVE` — so its nominal `hi` understates its true reach. That only
/// affects an interpolated quantile inside the top bucket, which is then clamped
/// to `max_ns` anyway.
#[inline]
const fn bucket_bounds(i: usize) -> (u64, u64) {
    if i < SUB_BUCKET_COUNT {
        return (i as u64, i as u64 + 1);
    }
    let k = i - SUB_BUCKET_COUNT;
    let octave = SUB_BUCKET_BITS + (k / SUB_BUCKET_COUNT) as u32;
    let part = (k % SUB_BUCKET_COUNT) as u64;
    let width = 1u64 << (octave - SUB_BUCKET_BITS);
    let lo = (1u64 << octave) + part * width;
    (lo, lo + width)
}

/// Fixed-size, non-allocating per-stage statistics: count, total, min, max,
/// plus an HDR-style sub-bucketed histogram for percentile estimation.
///
/// `count`, `sum_ns`, `min_ns` and `max_ns` are exact. Quantiles are estimated
/// from the histogram and carry at most 3.125% relative error (exactly, for
/// deltas below 32 ns); see [`HISTOGRAM_BUCKETS`].
#[derive(Clone, Copy, Debug)]
pub struct StageStats {
    pub count: u64,
    pub sum_ns: u64,
    pub min_ns: u64,
    pub max_ns: u64,
    /// Sub-bucketed delta counts; see [`HISTOGRAM_BUCKETS`] for the layout.
    /// Read quantiles out of it with [`StageStats::quantile_ns`] rather than
    /// indexing it directly — the bucket layout is not part of the API contract.
    pub histogram: [u64; HISTOGRAM_BUCKETS],
}

impl Default for StageStats {
    fn default() -> Self {
        Self {
            count: 0,
            sum_ns: 0,
            min_ns: u64::MAX,
            max_ns: 0,
            histogram: [0; HISTOGRAM_BUCKETS],
        }
    }
}

impl StageStats {
    #[inline]
    pub fn record(&mut self, delta_ns: u64) {
        self.count += 1;
        self.sum_ns = self.sum_ns.saturating_add(delta_ns);
        if delta_ns < self.min_ns {
            self.min_ns = delta_ns;
        }
        if delta_ns > self.max_ns {
            self.max_ns = delta_ns;
        }
        self.histogram[bucket_index(delta_ns)] += 1;
    }

    /// Mean delta in nanoseconds, or 0 if no samples recorded.
    pub fn mean_ns(&self) -> u64 {
        self.sum_ns.checked_div(self.count).unwrap_or(0)
    }

    /// Estimate the value at quantile `q` in `[0.0, 1.0]`, or 0 if no samples
    /// have been recorded. `q` outside `[0, 1]` is clamped.
    ///
    /// The rank is located in the histogram and then interpolated *within* its
    /// bucket, so the estimate sits inside the bucket rather than at its edge,
    /// and the result is clamped to the exactly-tracked `[min_ns, max_ns]`.
    /// Two properties follow, both of which the previous log2 implementation
    /// broke: the result is always a value the stage could actually have
    /// observed, and it is monotonic in `q`.
    pub fn quantile_ns(&self, q: f64) -> u64 {
        if self.count == 0 {
            return 0;
        }
        // Rank in 1..=count. A NaN `q` clamps to 0.0 and so reports the minimum.
        let rank = (((self.count as f64) * q.clamp(0.0, 1.0)).ceil() as u64).clamp(1, self.count);
        let mut cum = 0u64;
        for (i, &n) in self.histogram.iter().enumerate() {
            if n == 0 {
                continue;
            }
            cum += n;
            if cum < rank {
                continue;
            }
            let (lo, hi) = bucket_bounds(i);
            if hi - lo <= 1 {
                // Single-value bucket: the estimate is exact.
                return lo.clamp(self.min_ns, self.max_ns);
            }
            // Interpolate: place the rank at the midpoint of its share of the
            // bucket, so neither edge is systematically favoured.
            let within = rank - (cum - n);
            let frac = (within as f64 - 0.5) / n as f64;
            let estimate = lo as f64 + frac * (hi - lo) as f64;
            return (estimate as u64).clamp(self.min_ns, self.max_ns);
        }
        self.max_ns
    }
}

/// Record one observation's adjacent-stage deltas into `stages`.
///
/// Free-standing (rather than only a [`LatencyStats`] method) because the stage
/// list is not always known at compile time: the Python bindings aggregate over
/// a *runtime* `Vec<String>` of stage names, which cannot implement [`Latency`]
/// (`N` is a const and the names are `&'static`). Both aggregators call through
/// here so they agree on which samples count — a stage whose stamp, or whose
/// predecessor's, is unset (zero), or which went backwards, is skipped, so a
/// partially-stamped pipeline still yields useful numbers for the hops that did
/// stamp.
///
/// `stages[0]` is never written: stage 0 has no predecessor.
pub fn record_stage_deltas(stages: &mut [StageStats], stamps: &[u64]) {
    for i in 1..stages.len().min(stamps.len()) {
        let prev = stamps[i - 1];
        let cur = stamps[i];
        if prev == 0 || cur == 0 || cur < prev {
            continue;
        }
        stages[i].record(cur - prev);
    }
}

/// Render the multi-line per-hop summary printed at shutdown, for the stage
/// `names` and their accumulated `stages`.
///
/// The runtime-named counterpart of [`LatencyStats::format_report`], which
/// delegates here — one source of truth for a user-facing report format shared
/// by the Rust and Python surfaces.
pub fn format_latency_report(names: &[&str], stages: &[StageStats]) -> String {
    let mut out = String::new();
    out.push_str("latency report (delta from previous stage, nanoseconds):\n");
    out.push_str(&format!(
        "  {:<24} {:>10} {:>12} {:>12} {:>12} {:>12} {:>12}\n",
        "stage", "count", "min", "mean", "p50", "p99", "max"
    ));
    for i in 1..stages.len().min(names.len()) {
        let s = &stages[i];
        let label = format!("{} -> {}", names[i - 1], names[i]);
        if s.count == 0 {
            out.push_str(&format!("  {label:<24} {:>10}\n", "(no samples)"));
            continue;
        }
        out.push_str(&format!(
            "  {:<24} {:>10} {:>12} {:>12} {:>12} {:>12} {:>12}\n",
            label,
            s.count,
            s.min_ns,
            s.mean_ns(),
            s.quantile_ns(0.5),
            s.quantile_ns(0.99),
            s.max_ns,
        ));
    }
    out
}

/// Aggregated per-stage statistics for a [`Latency`] type.
///
/// Records the **delta from the previous stage** for stages 1..N. Stage 0 has
/// no predecessor and is not aggregated (its absolute timestamp is observable
/// directly on the message).
pub struct LatencyStats<L: Latency> {
    /// One slot per stage; `stages[0]` is unused (no predecessor).
    pub stages: Vec<StageStats>,
    _phantom: PhantomData<L>,
}

impl<L: Latency> Default for LatencyStats<L> {
    fn default() -> Self {
        Self {
            stages: vec![StageStats::default(); L::N],
            _phantom: PhantomData,
        }
    }
}

impl<L: Latency> LatencyStats<L> {
    pub fn new() -> Self {
        Self::default()
    }

    /// Record one observation. Computes deltas between adjacent stages and
    /// updates each stage's stats. Stages whose stamp is zero (unset) or
    /// whose predecessor is zero are skipped, so partial pipelines still
    /// produce useful numbers for the stages that did record stamps.
    pub fn observe(&mut self, latency: &L) {
        record_stage_deltas(&mut self.stages, latency.stamps());
    }

    /// Render a multi-line summary suitable for printing on shutdown.
    pub fn format_report(&self) -> String {
        format_latency_report(L::stage_names(), &self.stages)
    }
}

// ---------------------------------------------------------------------------
// Histogram tests
// ---------------------------------------------------------------------------
//
// The exact aggregates (count/sum/min/max) and the `Traced`/`Latency` layout are
// covered by `crates/wingfoil/tests/latency.rs` and the legacy tree's mirror.
// What follows is specific to the histogram: the bucket layout's structural
// invariants, and the quantile properties the previous log2 layout violated.

#[cfg(test)]
mod histogram_tests {
    use super::*;

    /// Values that exercise the exact region, every octave boundary, the
    /// interior of an octave, and the saturating top bucket.
    fn probe_values() -> Vec<u64> {
        let mut v: Vec<u64> = (0..40).collect();
        for octave in 5..40u32 {
            let base = 1u64 << octave;
            v.extend([base - 1, base, base + 1, base + base / 3, base * 2 - 1]);
        }
        v.push(u64::MAX);
        v
    }

    #[test]
    fn buckets_are_contiguous_and_cover_their_own_range() {
        // Bucket 0 starts at 0, and each bucket ends exactly where the next
        // begins — so no value falls between two buckets.
        assert_eq!(bucket_bounds(0).0, 0);
        for i in 0..HISTOGRAM_BUCKETS - 1 {
            let (lo, hi) = bucket_bounds(i);
            assert!(lo < hi, "bucket {i} is empty: [{lo}, {hi})");
            assert_eq!(hi, bucket_bounds(i + 1).0, "gap after bucket {i}");
        }
    }

    #[test]
    fn every_value_lands_in_a_bucket_containing_it() {
        for v in probe_values() {
            let i = bucket_index(v);
            assert!(i < HISTOGRAM_BUCKETS, "{v} indexed out of range: {i}");
            let (lo, hi) = bucket_bounds(i);
            if i == HISTOGRAM_BUCKETS - 1 {
                // The top bucket saturates: it absorbs everything from its own
                // lower bound upwards, so only `lo` is a real constraint.
                assert!(v >= lo, "{v} landed in the top bucket below its lo {lo}");
            } else {
                assert!(v >= lo && v < hi, "{v} not in bucket {i} = [{lo}, {hi})");
            }
        }
    }

    #[test]
    fn bucket_index_is_monotonic() {
        let mut probes = probe_values();
        probes.sort_unstable();
        let mut prev = 0usize;
        for v in probes {
            let i = bucket_index(v);
            assert!(
                i >= prev,
                "bucket_index({v}) = {i} went backwards from {prev}"
            );
            prev = i;
        }
    }

    #[test]
    fn relative_bucket_width_is_bounded_by_the_sub_bucket_resolution() {
        // The property that makes a quantile trustworthy: bucket width relative
        // to its own magnitude never exceeds 1/SUB_BUCKET_COUNT. The old log2
        // layout had width == lo, i.e. 100%.
        for i in SUB_BUCKET_COUNT..HISTOGRAM_BUCKETS {
            let (lo, hi) = bucket_bounds(i);
            let relative = (hi - lo) as f64 / lo as f64;
            assert!(
                relative <= 1.0 / SUB_BUCKET_COUNT as f64,
                "bucket {i} = [{lo}, {hi}) has relative width {relative}"
            );
        }
    }

    #[test]
    fn quantiles_are_exact_below_the_sub_bucket_count() {
        // The bottom region is one bucket per nanosecond, so no estimation.
        let mut s = StageStats::default();
        for _ in 0..100 {
            s.record(7);
        }
        assert_eq!(s.quantile_ns(0.5), 7);
        assert_eq!(s.quantile_ns(0.99), 7);
        assert_eq!(s.quantile_ns(1.0), 7);
    }

    #[test]
    fn quantiles_are_within_the_documented_error_bound() {
        // 1..=10_000 ns, so the true p50 is 5_000 and the true p99 is 9_900.
        let mut s = StageStats::default();
        for v in 1..=10_000u64 {
            s.record(v);
        }
        let tolerance = 1.0 / SUB_BUCKET_COUNT as f64;
        for (q, truth) in [(0.5, 5_000.0), (0.9, 9_000.0), (0.99, 9_900.0)] {
            let got = s.quantile_ns(q) as f64;
            let error = (got - truth).abs() / truth;
            assert!(
                error <= tolerance,
                "p{}: got {got}, true {truth}, relative error {error} > {tolerance}",
                q * 100.0
            );
        }
    }

    #[test]
    fn quantiles_never_escape_the_exact_min_max_range() {
        // The regression this replaces: the showcase report printed a p99 of
        // 262144 ns against an observed max of 164493, because the old
        // implementation returned the containing bucket's upper bound.
        let distributions: [&[u64]; 5] = [
            &[0],
            &[0, 0, 0, 0],
            &[75_075, 119_875, 131_000, 164_493],
            &[1, 2, 3, 1_000_000],
            &[u64::MAX, 1],
        ];
        for samples in distributions {
            let mut s = StageStats::default();
            for &v in samples {
                s.record(v);
            }
            for q in [0.0, 0.01, 0.5, 0.9, 0.99, 0.999, 1.0] {
                let got = s.quantile_ns(q);
                assert!(
                    got >= s.min_ns && got <= s.max_ns,
                    "q={q} gave {got}, outside [{}, {}] for {samples:?}",
                    s.min_ns,
                    s.max_ns
                );
            }
        }
    }

    #[test]
    fn all_zero_samples_report_zero_everywhere() {
        // The other half of the same report bug: p50 read 2 ns where min, mean
        // and max were all 0.
        let mut s = StageStats::default();
        for _ in 0..40 {
            s.record(0);
        }
        assert_eq!(s.min_ns, 0);
        assert_eq!(s.max_ns, 0);
        assert_eq!(s.mean_ns(), 0);
        assert_eq!(s.quantile_ns(0.5), 0);
        assert_eq!(s.quantile_ns(0.99), 0);
    }

    #[test]
    fn quantiles_are_monotonic_in_q() {
        let mut s = StageStats::default();
        for v in [1u64, 5, 17, 33, 64, 1_000, 25_000, 1_000_000, 40_000_000] {
            for _ in 0..11 {
                s.record(v);
            }
        }
        let mut prev = 0u64;
        for step in 0..=100 {
            let got = s.quantile_ns(step as f64 / 100.0);
            assert!(
                got >= prev,
                "q={} gave {got} after {prev}",
                step as f64 / 100.0
            );
            prev = got;
        }
    }

    #[test]
    fn out_of_range_and_nan_quantiles_are_clamped() {
        let mut s = StageStats::default();
        for v in 100..200u64 {
            s.record(v);
        }
        assert_eq!(s.quantile_ns(-1.0), s.quantile_ns(0.0));
        assert_eq!(s.quantile_ns(2.0), s.quantile_ns(1.0));
        assert_eq!(s.quantile_ns(f64::NAN), s.quantile_ns(0.0));
    }

    #[test]
    fn an_outlier_past_the_top_octave_saturates_without_losing_the_max() {
        let mut s = StageStats::default();
        s.record(1_000);
        s.record(u64::MAX);
        assert_eq!(
            s.max_ns,
            u64::MAX,
            "max_ns stays exact for a saturated sample"
        );
        assert_eq!(s.histogram[HISTOGRAM_BUCKETS - 1], 1);
        assert!(s.quantile_ns(1.0) <= s.max_ns);
    }

    #[test]
    fn quantile_is_zero_when_empty() {
        let s = StageStats::default();
        assert_eq!(s.quantile_ns(0.5), 0);
        assert_eq!(s.quantile_ns(1.0), 0);
    }
}
