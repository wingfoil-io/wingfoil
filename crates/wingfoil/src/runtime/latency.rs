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
    /// The embedded latency record's type — the stage layout this payload
    /// carries.
    type L: Latency;
    /// The embedded record, for reading stamps.
    fn latency(&self) -> &Self::L;
    /// The embedded record, for writing a stage's stamp.
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
    /// The value being carried. First, so the `#[repr(C)]` layout above holds.
    pub payload: T,
    /// The per-stage timestamps accumulated as this value crossed the
    /// pipeline.
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

/// Octaves above this saturate into the top bucket. `2^34` ns ≈ 17.2 s, and
/// `max_ns` still records the true value of an outlier that lands beyond it.
///
/// Every octave costs [`SUB_BUCKET_COUNT`] × 8 bytes in **every**
/// [`StageStats`], and a [`LatencyStats`] holds one per stage, so the ceiling
/// is a memory decision as much as a range one: at 34 a nine-stage record
/// carries 69 KB of histogram to describe hops measured in microseconds.
///
/// The floor under it is the **end-to-end row**, not the per-hop ones. A hop
/// worth a percentile is measured in microseconds, so a 1 s ceiling is beyond
/// any of them — but [`LatencyStats::total`] spans the whole schema, and a
/// pipeline whose stages straddle a WAN link (`trading_e2e`) reaches seconds
/// whenever one reconnects or stalls. A saturating total pins its quantiles
/// at `max_ns`, which reads as a plausible number rather than as an overflow,
/// so the ceiling has to clear the slowest row the schema can produce — and a
/// reconnect stall is measured in seconds to tens of seconds, which `2^32`
/// (4.29 s) does not clear. 34 does, for 512 bytes per stage over 33; the
/// per-stage cost is bounded by the size test below, and `StageStats` is no
/// longer `Copy`, so the bytes are paid once per stage rather than on every
/// by-value use.
const MAX_OCTAVE: u32 = 34;

/// Number of buckets in a [`StageStats`] histogram: the exact `[0, 32)` region
/// plus 32 divisions for each octave from `2^5` to `2^34`.
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
/// plus an HDR-style sub-bucketed histogram for percentile estimation, plus
/// the three **non-sample** tallies that say why an observation produced no
/// number at all.
///
/// `count`, `sum_ns`, `min_ns` and `max_ns` are exact. Quantiles are estimated
/// from the histogram and carry at most 3.125% relative error (exactly, for
/// deltas below 32 ns); see [`HISTOGRAM_BUCKETS`].
///
/// # Not `Copy`
///
/// The histogram dominates the struct — a `StageStats` is
/// [`HISTOGRAM_BUCKETS`] × 8 bytes plus a handful of counters, several
/// kilobytes in total. It was `Copy` once, which made every by-value use a
/// silent multi-kilobyte `memcpy`; it is `Clone`-only now so a copy has to be
/// asked for. Reach for [`summary`](Self::summary) when all you want is the
/// numbers — a [`HopStats`] is small and genuinely `Copy`.
#[derive(Clone, Debug)]
pub struct StageStats {
    /// Number of **measured** deltas recorded for this stage — strictly
    /// positive ones. Exact. See [`same_instant`](Self::same_instant) for the
    /// zero-delta observations, which are counted but not measured.
    pub count: u64,
    /// Total of every recorded delta, in nanoseconds; saturating. Exact.
    /// Divide by `count` for the mean.
    pub sum_ns: u64,
    /// Smallest delta seen, in nanoseconds. Exact. `u64::MAX` before the
    /// first observation.
    pub min_ns: u64,
    /// Largest delta seen, in nanoseconds. Exact. Zero before the first
    /// observation.
    pub max_ns: u64,
    /// Observations where both stamps were present and **identical**, i.e. the
    /// two stages ran in the same engine cycle and shared
    /// [`Ctx::wall_time`](crate::op::Ctx::wall_time)'s per-cycle snap.
    ///
    /// These are deliberately *not* folded into the histogram as 0 ns samples.
    /// A hop that is structurally unmeasurable is not a hop that took under a
    /// nanosecond, and recording it as one drags `min`, `mean` and `p50` to
    /// zero — which is exactly how a report can read `count 120, min 0, p99 0`
    /// for a pair of stages that were never separately timed at all. Stamp
    /// with [`Stamping::Precise`](crate::latency::Stamping::Precise) to turn
    /// these into real measurements.
    pub same_instant: u64,
    /// Observations rejected because the later stage's stamp *preceded* the
    /// earlier one's. In one process this cannot happen; across processes it
    /// is the signature of unsynchronised clocks, and a non-zero tally here is
    /// the difference between "this hop is fast" and "these numbers are
    /// meaningless".
    pub backwards: u64,
    /// Observations skipped because one of the two stamps was never written
    /// (still zero). Distinguishes "this hop is not instrumented" from "this
    /// hop is instrumented and every sample was thrown away".
    pub unstamped: u64,
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
            same_instant: 0,
            backwards: 0,
            unstamped: 0,
            histogram: [0; HISTOGRAM_BUCKETS],
        }
    }
}

impl StageStats {
    /// Fold one stage delta in: bumps `count`/`sum_ns`, widens the min/max,
    /// and increments the histogram bucket it falls in. Allocation-free, so
    /// it is safe on the graph path.
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

    /// Drop every sample and tally, returning the stage to its zeroed state.
    ///
    /// The building block of a **windowed** report: read a
    /// [`summary`](Self::summary), reset, and the next read describes only the
    /// period since. Cumulative-since-start statistics cannot do that — one
    /// outlier pins the p99 for the rest of the process's life, which is the
    /// difference between a gauge that tracks the system and one that records
    /// its worst moment forever.
    pub fn reset(&mut self) {
        *self = Self::default();
    }

    /// Every observation this stage saw, measured or not:
    /// `count + same_instant + backwards + unstamped`.
    ///
    /// Equal to the number of messages the report sink observed, for a stage
    /// index that exists on the record — so a `count` well below it says a
    /// large share of observations produced no measurement, and the three
    /// tallies say which kind.
    pub fn observations(&self) -> u64 {
        self.count + self.same_instant + self.backwards + self.unstamped
    }

    /// A small, `Copy` snapshot of this stage's numbers, labelled with the hop
    /// it describes.
    ///
    /// Quantiles are evaluated once, here, rather than on every read — which
    /// is what makes a [`HopStats`] cheap to pass around and to hold in a
    /// stream value, where a `StageStats` would drag its whole histogram
    /// along.
    pub fn summary(&self, from: &'static str, to: &'static str) -> HopStats {
        HopStats {
            from,
            to,
            ..self.summary_unlabelled()
        }
    }

    /// [`summary`](Self::summary) with empty labels — for an aggregator whose
    /// stage names are runtime strings rather than `&'static str` (the Python
    /// bindings), which applies its own labels afterwards. Keeping the numbers
    /// here means the sentinel rule and the quantile set exist in one place.
    pub fn summary_unlabelled(&self) -> HopStats {
        HopStats {
            from: "",
            to: "",
            count: self.count,
            // `min_ns` starts at the sentinel `u64::MAX` so the first sample
            // always wins; never leak that to a consumer.
            min_ns: if self.count == 0 { 0 } else { self.min_ns },
            mean_ns: self.mean_ns(),
            p50_ns: self.quantile_ns(0.5),
            p99_ns: self.quantile_ns(0.99),
            p999_ns: self.quantile_ns(0.999),
            max_ns: self.max_ns,
            same_instant: self.same_instant,
            backwards: self.backwards,
            unstamped: self.unstamped,
        }
    }
}

/// One hop's numbers at a point in time: small, `Copy`, and self-describing.
///
/// The read-out counterpart of [`StageStats`], which owns a multi-kilobyte
/// histogram and is the wrong thing to clone, hold in a stream value, or hand
/// to a metrics exporter. Quantiles are already evaluated, so reading one
/// costs nothing.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct HopStats {
    /// The stage this hop starts at.
    pub from: &'static str,
    /// The stage this hop ends at — the one whose slot accumulates it.
    pub to: &'static str,
    /// Measured deltas. See [`StageStats::count`].
    pub count: u64,
    /// Smallest measured delta, or 0 when `count == 0`.
    pub min_ns: u64,
    /// Mean measured delta, or 0 when `count == 0`.
    pub mean_ns: u64,
    /// Estimated median.
    pub p50_ns: u64,
    /// Estimated 99th percentile.
    pub p99_ns: u64,
    /// Estimated 99.9th percentile — the tail that matters once a system is
    /// under load, and the one a p99 alone hides.
    pub p999_ns: u64,
    /// Largest measured delta. Exact.
    pub max_ns: u64,
    /// Zero-delta observations. See [`StageStats::same_instant`].
    pub same_instant: u64,
    /// Backwards observations. See [`StageStats::backwards`].
    pub backwards: u64,
    /// Unstamped observations. See [`StageStats::unstamped`].
    pub unstamped: u64,
}

impl HopStats {
    /// `"from -> to"`, the label the report prints.
    pub fn label(&self) -> String {
        format!("{} -> {}", self.from, self.to)
    }

    /// Every observation, measured or not. See [`StageStats::observations`].
    pub fn observations(&self) -> u64 {
        self.count + self.same_instant + self.backwards + self.unstamped
    }
}

/// The index of the end-to-end total within a stage-statistics slice.
///
/// Stage 0 has no predecessor, so its slot would otherwise sit allocated and
/// permanently empty — several kilobytes of histogram describing nothing. It
/// holds the **end-to-end** delta instead: first declared stage to last, the
/// number every latency report is actually asked for and the one every caller
/// used to have to compute by hand.
pub const TOTAL: usize = 0;

/// Record one observation's per-hop deltas into `stages`, plus its end-to-end
/// total into `stages[`[`TOTAL`]`]`.
///
/// Free-standing (rather than only a [`LatencyStats`] method) because the stage
/// list is not always known at compile time: the Python bindings aggregate over
/// a *runtime* `Vec<String>` of stage names, which cannot implement [`Latency`]
/// (`N` is a const and the names are `&'static`). Both aggregators call through
/// here so they agree on which samples count.
///
/// # Every observation is accounted for
///
/// An observation that yields no measurement is **tallied, not dropped**, and
/// the three tallies are distinct because the fixes are:
///
/// | outcome | tally | what it means |
/// |---|---|---|
/// | `cur > prev` | [`count`](StageStats::count) | a real measurement |
/// | `cur == prev` | [`same_instant`](StageStats::same_instant) | both stages ran in one engine cycle and shared the cycle snap — stamp precisely to separate them |
/// | `cur < prev` | [`backwards`](StageStats::backwards) | the clocks disagree; the numbers are not comparable |
/// | either stamp is 0 | [`unstamped`](StageStats::unstamped) | this hop is not instrumented |
///
/// The zero-delta case is the one that used to be silently wrong: it was
/// recorded as a genuine 0 ns sample, so a pair of stages that shared a cycle
/// reported a full row of zeros — `count 120, min 0, mean 0, p50 0` —
/// indistinguishable from a hop measured at sub-nanosecond cost.
pub fn record_stage_deltas(stages: &mut [StageStats], stamps: &[u64]) {
    let n = stages.len().min(stamps.len());
    for i in 1..n {
        record_one(&mut stages[i], stamps[i - 1], stamps[i]);
    }
    // The end-to-end total, over the *declared* first and last stage rather
    // than the first and last that happen to be stamped: a statistic whose
    // endpoints move from message to message is not a statistic.
    if n >= 2 {
        let (first, last) = (stamps[0], stamps[n - 1]);
        record_one(&mut stages[TOTAL], first, last);
    }
}

/// Classify one `(earlier, later)` stamp pair into `stage` — the single place
/// the four outcomes above are decided, so a hop and the end-to-end total can
/// never disagree about what counts as a sample.
#[inline]
fn record_one(stage: &mut StageStats, prev: u64, cur: u64) {
    if prev == 0 || cur == 0 {
        stage.unstamped += 1;
    } else if cur < prev {
        stage.backwards += 1;
    } else if cur == prev {
        stage.same_instant += 1;
    } else {
        stage.record(cur - prev);
    }
}

/// Render the multi-line per-hop summary printed at shutdown, for the stage
/// `names` and their accumulated `stages`.
///
/// The runtime-named counterpart of [`LatencyStats::format_report`], which
/// delegates here — one source of truth for a user-facing report format shared
/// by the Rust and Python surfaces.
///
/// Three things the format guarantees, each of which the previous one did not:
///
/// * **A row with no measurements never prints numbers.** It prints `-` in
///   every statistic column and a note saying why — `same-cycle`, `backwards`
///   or `unstamped` — so a structurally unmeasurable hop cannot be mistaken for
///   a fast one.
/// * **The end-to-end total is a row.** It is the number the report is opened
///   for, and computing it from the per-hop rows is not possible (the hops that
///   were skipped are exactly the ones that would make the sum wrong).
/// * **The label column sizes itself** to the widest label, so long stage names
///   push the numeric columns across rather than out of alignment.
pub fn format_latency_report(names: &[&str], stages: &[StageStats]) -> String {
    let n = stages.len().min(names.len());
    let mut rows: Vec<(String, &StageStats)> = (1..n)
        .map(|i| (format!("{} -> {}", names[i - 1], names[i]), &stages[i]))
        .collect();
    if n >= 2 {
        rows.push((
            format!("{} -> {} (end to end)", names[0], names[n - 1]),
            &stages[TOTAL],
        ));
    }

    let width = rows
        .iter()
        .map(|(label, _)| label.len())
        .max()
        .unwrap_or(0)
        .max(24);

    let mut out = String::new();
    out.push_str("latency report (delta from previous stage, nanoseconds):\n");
    out.push_str(&format!(
        "  {:<width$} {:>10} {:>12} {:>12} {:>12} {:>12} {:>12} {:>12}\n",
        "stage",
        "count",
        "min",
        "mean",
        "p50",
        "p99",
        "p99.9",
        "max",
        width = width,
    ));
    for (label, s) in rows {
        let note = describe_non_samples(s);
        if s.count == 0 {
            // No measurement at all: dashes, not zeros. A zero here reads as
            // "immeasurably fast" when it means "never measured".
            out.push_str(&format!(
                "  {:<width$} {:>10} {:>12} {:>12} {:>12} {:>12} {:>12} {:>12}{note}\n",
                label,
                0,
                "-",
                "-",
                "-",
                "-",
                "-",
                "-",
                width = width,
            ));
            continue;
        }
        out.push_str(&format!(
            "  {:<width$} {:>10} {:>12} {:>12} {:>12} {:>12} {:>12} {:>12}{note}\n",
            label,
            s.count,
            s.min_ns,
            s.mean_ns(),
            s.quantile_ns(0.5),
            s.quantile_ns(0.99),
            s.quantile_ns(0.999),
            s.max_ns,
            width = width,
        ));
    }
    out
}

/// The trailing note on a report row: the tallies of observations that
/// produced no measurement, or the empty string when every observation did.
///
/// Kept out of the aligned numeric columns deliberately — it is a diagnosis,
/// not a statistic, and it is absent on a healthy row.
fn describe_non_samples(s: &StageStats) -> String {
    let mut parts: Vec<String> = Vec::new();
    if s.same_instant > 0 {
        parts.push(format!("{} same-cycle", s.same_instant));
    }
    if s.backwards > 0 {
        parts.push(format!("{} backwards", s.backwards));
    }
    if s.unstamped > 0 {
        parts.push(format!("{} unstamped", s.unstamped));
    }
    if parts.is_empty() {
        if s.count == 0 {
            return "  (never observed)".to_string();
        }
        return String::new();
    }
    format!("  ({})", parts.join(", "))
}

/// Aggregated per-stage statistics for a [`Latency`] type.
///
/// Records the **delta from the previous stage** for stages 1..N, plus the
/// end-to-end delta in `stages[`[`TOTAL`]`]` — stage 0 has no predecessor, so
/// its slot carries the total rather than sitting empty.
///
/// Prefer the accessors — [`hops`](Self::hops), [`total`](Self::total),
/// [`snapshot`](Self::snapshot) — over indexing `stages` directly. Raw
/// indexing means reproducing the `1..N` convention (and the `stages[0]` rule)
/// at every call site, which is an off-by-one waiting to happen.
#[derive(Clone)]
pub struct LatencyStats<L: Latency> {
    /// One slot per stage: `stages[i]` is the hop from stage `i - 1` to stage
    /// `i`, and `stages[`[`TOTAL`]`]` is the end-to-end total.
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
    /// Zeroed statistics for every stage `L` declares.
    pub fn new() -> Self {
        Self::default()
    }

    /// Record one observation: the delta between each adjacent pair of stages,
    /// plus the end-to-end total. Every observation lands in exactly one tally
    /// — see [`record_stage_deltas`] for the four outcomes — so a partially
    /// stamped pipeline yields useful numbers for the hops that did stamp, and
    /// says so for the ones that did not.
    pub fn observe(&mut self, latency: &L) {
        record_stage_deltas(&mut self.stages, latency.stamps());
    }

    /// Render a multi-line summary suitable for printing on shutdown.
    pub fn format_report(&self) -> String {
        format_latency_report(L::stage_names(), &self.stages)
    }

    /// The end-to-end summary: first declared stage to last.
    pub fn total(&self) -> HopStats {
        let names = L::stage_names();
        let n = self.stages.len().min(names.len());
        if n < 2 {
            return HopStats::default();
        }
        self.stages[TOTAL].summary(names[0], names[n - 1])
    }

    /// Every hop, in stamp order, as small labelled summaries — no `TOTAL`
    /// slot and no `1..N` convention for the caller to get wrong.
    pub fn hops(&self) -> Vec<HopStats> {
        let names = L::stage_names();
        (1..self.stages.len().min(names.len()))
            .map(|i| self.stages[i].summary(names[i - 1], names[i]))
            .collect()
    }

    /// A point-in-time read of every hop plus the total, cheap enough to hold
    /// in a stream value or hand to a metrics exporter.
    pub fn snapshot(&self) -> LatencySnapshot {
        LatencySnapshot {
            total: self.total(),
            hops: self.hops(),
        }
    }

    /// Drop every sample and tally, keeping the stage layout.
    ///
    /// Statistics are cumulative for the life of a run unless something resets
    /// them, so a single outlier pins the p99 forever. Snapshot-then-reset on
    /// a timer turns the same accumulator into per-window statistics — see
    /// [`LatencyHandle::windows`](crate::latency::LatencyHandle::windows),
    /// which wires exactly that.
    pub fn reset(&mut self) {
        for stage in &mut self.stages {
            stage.reset();
        }
    }
}

/// A point-in-time read of a whole latency schema: every hop plus the
/// end-to-end total, as small `Copy` summaries.
///
/// This is the shape latency leaves the engine in — what
/// [`LatencyHandle::windows`](crate::latency::LatencyHandle::windows)
/// emits as a stream, and what a metrics exporter or an alert wants. A
/// [`LatencyStats`] is the accumulator, several kilobytes per stage; a
/// snapshot is the answer.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct LatencySnapshot {
    /// First declared stage to last.
    pub total: HopStats,
    /// One entry per hop, in stamp order.
    pub hops: Vec<HopStats>,
}

impl LatencySnapshot {
    /// The hop ending at stage `to`, or `None` if no such hop exists.
    pub fn hop(&self, to: &str) -> Option<&HopStats> {
        self.hops.iter().find(|h| h.to == to)
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

// ---------------------------------------------------------------------------
// Aggregation tests
// ---------------------------------------------------------------------------
//
// The histogram tests above cover bucket layout and quantiles. These cover
// what an *observation* turns into: which of the four outcomes it lands in,
// the end-to-end total, and what the report says about each.

#[cfg(test)]
mod aggregation_tests {
    use super::*;

    /// A four-stage schema's worth of slots.
    fn slots() -> Vec<StageStats> {
        vec![StageStats::default(); 4]
    }

    #[test]
    fn a_measured_hop_records_a_sample_and_no_tally() {
        let mut stages = slots();
        record_stage_deltas(&mut stages, &[100, 110, 130, 160]);
        assert_eq!(1, stages[1].count);
        assert_eq!(10, stages[1].min_ns);
        assert_eq!(0, stages[1].same_instant);
        assert_eq!(0, stages[1].backwards);
        assert_eq!(0, stages[1].unstamped);
    }

    #[test]
    fn a_same_cycle_hop_is_tallied_not_recorded_as_a_zero_sample() {
        // The bug this replaces: two stages sharing an engine cycle produced a
        // genuine 0 ns sample, so a hop that was never separately timed
        // reported `count 40, min 0, mean 0, p50 0` — indistinguishable from a
        // hop measured at sub-nanosecond cost.
        let mut stages = slots();
        for _ in 0..40 {
            record_stage_deltas(&mut stages, &[100, 100, 100, 100]);
        }
        assert_eq!(0, stages[1].count, "no measurement was possible");
        assert_eq!(40, stages[1].same_instant);
        assert_eq!(0, stages[1].mean_ns());
        assert_eq!(40, stages[1].observations(), "the observation is not lost");
    }

    #[test]
    fn a_backwards_hop_is_tallied_and_never_measured() {
        let mut stages = slots();
        record_stage_deltas(&mut stages, &[500, 400, 600, 700]);
        assert_eq!(0, stages[1].count);
        assert_eq!(1, stages[1].backwards);
        // The hops either side of the skew still measure normally.
        assert_eq!(1, stages[2].count);
        assert_eq!(1, stages[3].count);
    }

    #[test]
    fn an_unstamped_hop_is_distinguishable_from_a_rejected_one() {
        let mut stages = slots();
        record_stage_deltas(&mut stages, &[100, 0, 0, 160]);
        assert_eq!(1, stages[1].unstamped, "stage 1 never stamped");
        assert_eq!(1, stages[2].unstamped, "neither did stage 2");
        assert_eq!(1, stages[3].unstamped, "so its predecessor is missing");
        assert_eq!(0, stages[1].backwards);
    }

    #[test]
    fn the_total_slot_carries_first_stage_to_last() {
        let mut stages = slots();
        record_stage_deltas(&mut stages, &[100, 110, 130, 160]);
        assert_eq!(1, stages[TOTAL].count);
        assert_eq!(60, stages[TOTAL].min_ns, "160 - 100, not a sum of hops");
    }

    #[test]
    fn the_total_is_measured_even_when_a_middle_hop_is_not() {
        // The end-to-end number survives a partially instrumented interior —
        // and could not be recovered by summing the hops, since the hop that
        // was skipped is exactly the one that would make the sum wrong.
        let mut stages = slots();
        record_stage_deltas(&mut stages, &[100, 0, 0, 160]);
        assert_eq!(1, stages[TOTAL].count);
        assert_eq!(60, stages[TOTAL].min_ns);
    }

    #[test]
    fn the_total_is_unstamped_when_an_endpoint_is() {
        let mut stages = slots();
        record_stage_deltas(&mut stages, &[0, 110, 130, 160]);
        assert_eq!(0, stages[TOTAL].count);
        assert_eq!(1, stages[TOTAL].unstamped);
    }

    #[test]
    fn reset_returns_a_stage_to_its_zeroed_state() {
        let mut s = StageStats::default();
        s.record(1_000);
        s.same_instant = 3;
        s.reset();
        assert_eq!(0, s.count);
        assert_eq!(0, s.same_instant);
        assert_eq!(u64::MAX, s.min_ns, "the sentinel is restored, not zeroed");
        assert_eq!(0, s.quantile_ns(0.99));
        assert_eq!(StageStats::default().histogram, s.histogram);
    }

    #[test]
    fn a_stage_stays_small_enough_to_hold_one_per_stage() {
        // The histogram dominates `StageStats`, and a `LatencyStats` holds one
        // per stage — nine stages is nine of these, ~69 KB of histogram. That
        // is affordable because `StageStats` is no longer `Copy` (each is paid
        // for once), but it is the budget to re-check before raising
        // `MAX_OCTAVE` further.
        let size = std::mem::size_of::<StageStats>();
        assert_eq!(
            HISTOGRAM_BUCKETS, 960,
            "the octave ceiling moved; re-check the size budget"
        );
        assert!(
            size <= 8 * 1024,
            "StageStats grew to {size} bytes; raising MAX_OCTAVE costs \
             SUB_BUCKET_COUNT * 8 bytes per stage per octave"
        );
    }

    #[test]
    fn a_summary_never_leaks_the_min_sentinel() {
        let empty = StageStats::default().summary("a", "b");
        assert_eq!(0, empty.min_ns);
        assert_eq!(0, empty.count);
        assert_eq!("a -> b", empty.label());
    }

    #[test]
    fn the_report_prints_dashes_and_a_reason_where_it_has_no_measurement() {
        let names = ["produce", "publish", "receive"];
        let mut stages = slots();
        stages.truncate(3);
        for _ in 0..120 {
            // produce and publish share a cycle; publish -> receive is real.
            record_stage_deltas(&mut stages, &[100, 100, 90_000]);
        }
        let report = format_latency_report(&names, &stages);

        let same_cycle_row = report
            .lines()
            .find(|l| l.contains("produce -> publish"))
            .expect("the same-cycle row is printed");
        assert!(
            same_cycle_row.contains('-') && same_cycle_row.contains("120 same-cycle"),
            "a hop with no measurement must say so, not print zeros: {same_cycle_row}"
        );
        assert_eq!(
            6,
            same_cycle_row
                .split_whitespace()
                .filter(|token| *token == "-")
                .count(),
            "min/mean/p50/p99/p99.9/max are all dashes, not zeros: {same_cycle_row}"
        );

        let measured_row = report
            .lines()
            .find(|l| l.contains("publish -> receive"))
            .expect("the measured row is printed");
        assert!(measured_row.contains("120"), "{measured_row}");
        assert!(
            !measured_row.contains("same-cycle"),
            "a healthy row carries no note: {measured_row}"
        );
    }

    #[test]
    fn the_report_ends_with_an_end_to_end_row() {
        let names = ["ingest", "decode", "publish"];
        let mut stages = slots();
        stages.truncate(3);
        record_stage_deltas(&mut stages, &[100, 150, 400]);
        let report = format_latency_report(&names, &stages);
        let total = report
            .lines()
            .last()
            .expect("the report has a last line")
            .to_string();
        assert!(
            total.contains("ingest -> publish (end to end)"),
            "the total row names both endpoints: {total}"
        );
        assert!(
            total.contains("300"),
            "and carries the measurement: {total}"
        );
    }

    #[test]
    fn the_report_has_a_p999_column_and_a_self_sizing_label_column() {
        let names = ["a_very_long_stage_name_indeed", "and_another_long_one"];
        let mut stages = vec![StageStats::default(); 2];
        record_stage_deltas(&mut stages, &[100, 200]);
        let report = format_latency_report(&names, &stages);
        let header = report.lines().nth(1).expect("a header row");
        assert!(header.contains("p99.9"), "{header}");
        // Every row lines its numeric columns up under the header's.
        let count_column = header.find("count").expect("a count column");
        for line in report.lines().skip(2) {
            assert!(
                line.len() > count_column,
                "row is shorter than the header's first numeric column: {line}"
            );
        }
    }
}
