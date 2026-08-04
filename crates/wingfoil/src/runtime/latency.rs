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

/// Bits of sub-bucket resolution within each octave: `2^8 = 256` divisions, so
/// no reported quantile carries more than `1/256` ≈ **0.39%** relative error.
/// See [`QUANTILE_RELATIVE_ERROR`].
///
/// **Why 8 and not 5.** At 5 bits the bound was 3.125%, which is fine for a
/// dashboard and useless as a regression gate: the thing you want to catch in
/// CI or in a canary is a **2% p99 regression**, and a histogram whose buckets
/// are 3.125% wide cannot distinguish that from noise — two runs either side of
/// a real regression can land in the same bucket and report the identical
/// number. Tail work is worse still: p99.9 on a heavy-tailed hop lands in the
/// sparse region where one bucket may hold a single sample, so the reported
/// value is the bucket, not the sample.
///
/// 8 bits is ~2.4 significant digits. HdrHistogram's convention is 3 significant
/// digits (0.1%, 10 bits), which here would mean 25,600 buckets and 205 KiB per
/// stage — the point where a per-session aggregator starts to cost real memory.
/// 8 bits keeps the bound comfortably under any regression worth alerting on at
/// 55 KiB per stage (see [`HISTOGRAM_BUCKETS`]).
const SUB_BUCKET_BITS: u32 = 8;

/// The worst-case relative error of any value read out of a [`StageStats`]
/// histogram — `1 / 2^SUB_BUCKET_BITS`.
///
/// Exposed so a caller gating on a percentile can size its threshold against
/// the instrument rather than guessing: a regression gate should trip on a
/// change comfortably larger than this, and a change smaller than this is not
/// resolvable from a histogram at all (use `min`/`mean`/`max`, which are
/// exact, or raise the resolution).
pub const QUANTILE_RELATIVE_ERROR: f64 = 1.0 / SUB_BUCKET_COUNT as f64;

/// Divisions per octave — also the size of the exact, single-nanosecond region
/// at the bottom of the range (`[0, 256)`), where a bucket is one value wide and
/// the quantile is therefore exact. Note that widening
/// [`SUB_BUCKET_BITS`] widens this exact region too, so every in-process hop
/// under 256 ns — which is most of them, an engine cycle being ~27 ns/node — is
/// now reported exactly rather than estimated.
const SUB_BUCKET_COUNT: usize = 1 << SUB_BUCKET_BITS;

/// Octaves above this saturate into the top bucket. `2^34` ns ≈ 17.2 s — beyond
/// any per-hop latency worth a percentile, and `max_ns` still records the true
/// value of an outlier that lands there.
const MAX_OCTAVE: u32 = 34;

/// Number of buckets in a [`StageStats`] histogram: the exact `[0, 256)` region
/// plus [`SUB_BUCKET_COUNT`] divisions for each octave from `2^8` to
/// `2^MAX_OCTAVE`. 6,912 buckets, so a [`StageStats`] is ~55 KiB — heap-held
/// (`LatencyStats::stages` is a `Vec`), one per stage per aggregator.
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
/// from the histogram and carry at most [`QUANTILE_RELATIVE_ERROR`] (≈0.39%)
/// relative error — exactly, for deltas below 256 ns; see
/// [`HISTOGRAM_BUCKETS`].
///
/// **Not `Copy`.** At ~55 KiB it is far too large to want an implicit memcpy on
/// every use; it is `Clone` so the explicit copies (`vec![..; n]`,
/// `slice::fill`) still work and are visible at the call site.
#[derive(Clone, Debug)]
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
        "  {:<24} {:>10} {:>12} {:>12} {:>12} {:>12} {:>12} {:>12}\n",
        "stage", "count", "min", "mean", "p50", "p99", "p99.9", "max"
    ));
    for i in 1..stages.len().min(names.len()) {
        let s = &stages[i];
        let label = format!("{} -> {}", names[i - 1], names[i]);
        if s.count == 0 {
            out.push_str(&format!("  {label:<24} {:>10}\n", "(no samples)"));
            continue;
        }
        out.push_str(&format!(
            "  {:<24} {:>10} {:>12} {:>12} {:>12} {:>12} {:>12} {:>12}\n",
            label,
            s.count,
            s.min_ns,
            s.mean_ns(),
            s.quantile_ns(0.5),
            s.quantile_ns(0.99),
            s.quantile_ns(0.999),
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

    /// The resolution is the point of the histogram, so pin it rather than
    /// leave it implied by `SUB_BUCKET_BITS`. A future narrowing has to change
    /// this number deliberately.
    #[test]
    fn the_documented_error_bound_is_the_one_the_layout_delivers() {
        assert_eq!(1.0 / 256.0, QUANTILE_RELATIVE_ERROR);
        assert_eq!(6_912, HISTOGRAM_BUCKETS);
        // Every *estimating* bucket honours it, including the widest (the
        // bottom of an octave, where `hi - lo` is largest relative to `lo`).
        // The `[0, 256)` region is skipped because it does not estimate at all:
        // those buckets are one value wide, so the quantile is exact — a
        // "relative width" of 100% at `lo == 1` describes an exact answer.
        for i in SUB_BUCKET_COUNT..HISTOGRAM_BUCKETS {
            let (lo, hi) = bucket_bounds(i);
            assert!(
                (hi - lo) as f64 / lo as f64 <= QUANTILE_RELATIVE_ERROR,
                "bucket {i} = [{lo}, {hi}) is wider than the documented bound"
            );
        }
    }

    /// **The regression-gate property.** A 2% shift in the tail — the smallest
    /// thing worth alerting on — has to move the reported p99, or the histogram
    /// is not an instrument you can gate on.
    ///
    /// The values are chosen to make this a real guard rather than a
    /// coincidence, in two ways.
    ///
    /// **The tail sits near the bottom of an octave.** 65,600 ns is just above
    /// `2^16`, where a bucket is widest in relative terms. At the old 5 bits
    /// that bucket is `[65536, 67584)` — 2,048 ns wide — and both 65,600 and
    /// 65,600 × 1.02 = 66,912 fall inside it, so the two runs interpolate to
    /// the *identical* p99. At 8 bits the bucket is 256 ns and they separate.
    ///
    /// **A far outlier lifts `max_ns` out of the way.** `quantile_ns` clamps
    /// its estimate to the exactly-tracked `[min_ns, max_ns]`, and with a
    /// single-valued tail that clamp lands on `max_ns` — which *is* the tail
    /// value, so the two runs differ for a reason that has nothing to do with
    /// the histogram. (An earlier draft of this test passed at 5 bits for
    /// exactly that reason.) The 5 ms outlier puts `max_ns` far above the p99,
    /// so the clamp is inert and the number under test is the interpolation.
    ///
    /// Reverting `SUB_BUCKET_BITS` to 5 fails this test.
    #[test]
    fn a_two_percent_tail_regression_moves_the_reported_p99() {
        /// 9,000 fast samples, 1,000 at `tail_ns`, and 10 far outliers. The p99
        /// rank (9,910 of 10,010) falls inside the tail group, so the true p99
        /// is `tail_ns`; the outliers only raise `max_ns`.
        fn p99_of(tail_ns: u64) -> u64 {
            let mut s = StageStats::default();
            for _ in 0..9_000 {
                s.record(1_000);
            }
            for _ in 0..1_000 {
                s.record(tail_ns);
            }
            for _ in 0..10 {
                s.record(5_000_000);
            }
            s.quantile_ns(0.99)
        }

        let before = p99_of(65_600);
        let after = p99_of(66_912); // +2%, and inside the same 5-bit bucket
        assert!(
            after > before,
            "a 2% tail regression was invisible: p99 {before} -> {after}"
        );
        // And the reported move is close to the real one, not merely non-zero.
        let reported = (after - before) as f64 / before as f64;
        assert!(
            (reported - 0.02).abs() <= 2.0 * QUANTILE_RELATIVE_ERROR,
            "p99 moved {reported:.4}, expected ~0.02"
        );
    }

    /// The error bound as an **absolute** number rather than one derived from
    /// `SUB_BUCKET_COUNT`.
    ///
    /// `quantiles_are_within_the_documented_error_bound` above computes its
    /// tolerance from the constant, so it holds at any resolution and cannot
    /// notice the resolution being narrowed. This one spells 0.39% out, on a
    /// distribution whose tail is a single value near an octave floor (the
    /// worst case for bucket width) with `max_ns` lifted clear of the clamp.
    #[test]
    fn p99_tracks_truth_to_the_absolute_documented_bound() {
        let mut s = StageStats::default();
        for _ in 0..9_000 {
            s.record(1_000);
        }
        for _ in 0..1_000 {
            s.record(65_600);
        }
        for _ in 0..10 {
            s.record(5_000_000);
        }

        let got = s.quantile_ns(0.99) as f64;
        let error = (got - 65_600.0).abs() / 65_600.0;
        assert!(
            error <= 1.0 / 256.0,
            "p99 {got} vs true 65600: relative error {error:.5} exceeds 1/256"
        );
    }

    /// p99.9 has to resolve *separately* from p99 — that is the whole reason
    /// for the extra column. With a distribution whose 99th and 99.9th
    /// percentiles are an order of magnitude apart, reporting them as one
    /// number would be a failure.
    #[test]
    fn p999_resolves_the_far_tail_separately_from_p99() {
        // 10k samples split so the two ranks land in different groups: the p99
        // rank (9,900) is the last of the 50 µs group, the p99.9 rank (9,990)
        // is inside the 500 µs group.
        let mut s = StageStats::default();
        for _ in 0..8_900 {
            s.record(1_000);
        }
        for _ in 0..1_000 {
            s.record(50_000);
        }
        for _ in 0..100 {
            s.record(500_000);
        }

        let p99 = s.quantile_ns(0.99);
        let p999 = s.quantile_ns(0.999);
        assert!(
            p999 > p99 * 5,
            "p99.9 ({p999}) did not separate from p99 ({p99})"
        );
        for (q, truth, got) in [(0.99, 50_000.0, p99), (0.999, 500_000.0, p999)] {
            let error = (got as f64 - truth).abs() / truth;
            assert!(
                error <= QUANTILE_RELATIVE_ERROR,
                "p{}: got {got}, true {truth}, relative error {error}",
                q * 100.0
            );
        }
    }

    /// Every in-process hop is now recorded exactly: the one-bucket-per-ns
    /// region reaches 256 ns, and an engine cycle is ~27 ns per node.
    #[test]
    fn sub_microsecond_hops_are_exact_not_estimated() {
        for v in [1u64, 27, 128, 255] {
            let mut s = StageStats::default();
            for _ in 0..1_000 {
                s.record(v);
            }
            assert_eq!(v, s.quantile_ns(0.5));
            assert_eq!(v, s.quantile_ns(0.99));
            assert_eq!(v, s.quantile_ns(0.999));
        }
    }

    /// The report gained a `p99.9` column between `p99` and `max`; the header
    /// and the rows must stay in step (they are formatted separately).
    #[test]
    fn the_report_carries_p999_between_p99_and_max() {
        // 1,000 samples: the p99 rank (990) is the last fast one, the p99.9
        // rank (999) is inside the slow tail — so the two columns must differ.
        let mut s = StageStats::default();
        for _ in 0..990 {
            s.record(100);
        }
        for _ in 0..10 {
            s.record(900_000);
        }
        let report = format_latency_report(&["a", "b"], &[StageStats::default(), s]);

        let header = report.lines().nth(1).expect("header row");
        let p99_at = header.find("p99").expect("p99 column");
        let p999_at = header.find("p99.9").expect("p99.9 column");
        let max_at = header.find("max").expect("max column");
        assert!(
            p99_at < p999_at && p999_at < max_at,
            "columns out of order: {header}"
        );

        let row = report.lines().nth(2).expect("stage row");
        let cells: Vec<&str> = row.split_whitespace().collect();
        // label is "a -> b" (3 tokens), then count, min, mean, p50, p99, p99.9, max
        assert_eq!(10, cells.len(), "unexpected row shape: {row}");
        assert_eq!("900000", cells[9], "max column");
        assert_eq!("900000", cells[8], "p99.9 sits on the single outlier");
        assert_eq!("100", cells[7], "p99 sits below it");
    }
}
