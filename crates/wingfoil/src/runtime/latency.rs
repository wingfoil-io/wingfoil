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
//! See [`runtime`](super) for why the shared core lives on the next side.

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

const HISTOGRAM_BUCKETS: usize = 64;

/// Fixed-size, non-allocating per-stage statistics: count, total, min, max,
/// plus a log2-bucketed histogram for percentile estimation.
#[derive(Clone, Copy, Debug)]
pub struct StageStats {
    pub count: u64,
    pub sum_ns: u64,
    pub min_ns: u64,
    pub max_ns: u64,
    /// `histogram[i]` counts deltas in `[2^i ns, 2^(i+1) ns)`.
    /// Index 0 covers `[0, 2 ns)`.
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
        // Log2 bucket: bucket = ilog2(delta_ns + 1), capped to HISTOGRAM_BUCKETS-1.
        let bucket = ((delta_ns + 1).ilog2() as usize).min(HISTOGRAM_BUCKETS - 1);
        self.histogram[bucket] += 1;
    }

    /// Mean delta in nanoseconds, or 0 if no samples recorded.
    pub fn mean_ns(&self) -> u64 {
        self.sum_ns.checked_div(self.count).unwrap_or(0)
    }

    /// Estimate the value at quantile `q` in `[0.0, 1.0]` from the histogram.
    /// Returns the upper bound of the bucket containing the quantile, or 0 if
    /// no samples have been recorded.
    pub fn quantile_ns(&self, q: f64) -> u64 {
        if self.count == 0 {
            return 0;
        }
        let target = ((self.count as f64) * q).ceil() as u64;
        let mut cum = 0u64;
        for (i, &n) in self.histogram.iter().enumerate() {
            cum += n;
            if cum >= target {
                // Upper bound of bucket i is 2^(i+1).
                return 1u64 << (i + 1).min(63);
            }
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
