//! Helpers shared across I/O adapters — a faithful port of the classic
//! `wingfoil::adapters::common` module.
//!
//! Everything here is a **pure helper**, not a graph node: there is no [`Op`],
//! no `cycle`, no source or sink. [`TimeWindow`] / [`WindowFilter`] are the
//! out-of-window row filter used by historical/replay sources, and
//! [`compute_time_slices`] / [`compute_validated_time_slices`] split a run's
//! `[start, end)` window into per-query slices. They are the building blocks a
//! time-sliced replay adapter (a future kdb/postgres port) composes *inside*
//! its own source closure — the port keeps them as plain functions/structs, so
//! the values they produce are identical to the classic crate's.
//!
//! The graph clock is strictly monotonic and bounded to the run's
//! `[start_time, end_time)`: delivering a value before the clock aborts the run
//! ("received Historical message but with time less than graph time"), and can
//! even underflow `NanoTime` subtraction in debug builds. Any adapter that
//! replays a caller-parameterised range (time-sliced queries, cursor replays,
//! etc.) can over-read its bounds, so it should pass each emitted row through a
//! [`WindowFilter`] first.
//!
//! # Deviation from classic
//!
//! [`WindowFilter::finish`] emits its summary via `eprintln!` to stderr rather
//! than `log::warn!` — wingfoil-next has no `log` dependency (the same
//! deviation the ported `timed` op documents). The values and drop-counting
//! behaviour are identical.
//!
//! [`Op`]: crate::op::Op

use wingfoil::NanoTime;

/// A half-open on-graph time window `[lo, hi)`.
#[derive(Clone, Copy, Debug)]
pub struct TimeWindow {
    lo: NanoTime,
    hi: NanoTime,
}

impl TimeWindow {
    /// Clamp a candidate range `[t0, t1)` to the run bounds `[start, end)`.
    ///
    /// Use when a source's natural boundaries (`t0`, `t1`) may fall outside the
    /// requested range — e.g. period-aligned query slices whose first `t0`
    /// precedes `start`, or a final slice whose `t1` overshoots `end`.
    pub fn clamp(t0: NanoTime, t1: NanoTime, start: NanoTime, end: NanoTime) -> Self {
        Self {
            lo: t0.max(start),
            hi: t1.min(end),
        }
    }

    /// True if `time` is within `[lo, hi)`.
    pub fn contains(&self, time: NanoTime) -> bool {
        self.lo <= time && time < self.hi
    }

    /// The window's inclusive lower bound.
    pub fn lo(&self) -> NanoTime {
        self.lo
    }

    /// The window's exclusive upper bound.
    pub fn hi(&self) -> NanoTime {
        self.hi
    }
}

/// Filters rows to a [`TimeWindow`], counting discards and emitting a single
/// summary warning.
///
/// Create one per slice/batch, call [`WindowFilter::keep`] for each row before
/// emitting it, and [`WindowFilter::finish`] once when the batch is drained:
///
/// ```ignore
/// let mut filter = WindowFilter::new("my_adapter", TimeWindow::clamp(t0, t1, start, end));
/// for (time, record) in rows {
///     if !filter.keep(time) { continue; }
///     yield Ok((time, record));
/// }
/// filter.finish();
/// ```
pub struct WindowFilter {
    label: &'static str,
    window: TimeWindow,
    dropped: usize,
}

impl WindowFilter {
    pub fn new(label: &'static str, window: TimeWindow) -> Self {
        Self {
            label,
            window,
            dropped: 0,
        }
    }

    /// Returns `true` if the row at `time` should be emitted; otherwise records
    /// a discard.
    pub fn keep(&mut self, time: NanoTime) -> bool {
        if self.window.contains(time) {
            true
        } else {
            self.dropped += 1;
            false
        }
    }

    /// Number of rows discarded so far (out-of-window). Exposed for parity
    /// testing without having to observe the stderr summary.
    pub fn dropped(&self) -> usize {
        self.dropped
    }

    /// Emit a single summary warning if any rows were discarded. Consumes `self`
    /// so it is called exactly once per batch.
    ///
    /// Deviation from classic: prints to stderr (`eprintln!`) rather than
    /// `log::warn!`, since wingfoil-next carries no `log` dependency.
    pub fn finish(self) {
        if self.dropped > 0 {
            eprintln!(
                "{}: dropped {} row(s) outside the requested window [{:?}, {:?}); \
                the source returned data beyond the range it was asked for",
                self.label, self.dropped, self.window.lo, self.window.hi,
            );
        }
    }
}

// ---- Time slicing --------------------------------------------------------
//
// Split the run's `[start, end)` window into contiguous, half-open slices, one
// query per slice — the shared front door for time-partitioned historical
// readers (classic `kdb_read` / `kdb_read_cached` / `postgres_read`). Ported
// un-gated and `pub` (the classic crate gates these to its `kdb`/`postgres`
// features and keeps them `pub(crate)`); wingfoil-next has no such adapters
// yet, so they are exposed as public helpers rather than left as dead code.

/// Validate the run window and period, then compute the time slices.
///
/// Shared front door for time-sliced readers: enforces the preconditions of
/// [`compute_time_slices`] with uniform error messages (prefixed with `adapter`
/// so callers see the function they actually called), then delegates to it.
///
/// * `period` must be non-zero (a zero period cannot advance through the window).
/// * `start_time` must be non-zero — i.e. `RunMode::HistoricalFrom` with an explicit start.
/// * `end_time` must be a bounded time from `RunFor::Duration`; `Forever` arrives as
///   `Ok(NanoTime::MAX)` and `Cycles` as `Err`, both rejected.
pub fn compute_validated_time_slices(
    adapter: &str,
    start_time: NanoTime,
    end_time: anyhow::Result<NanoTime>,
    period: std::time::Duration,
) -> anyhow::Result<Vec<((NanoTime, NanoTime), i32, usize)>> {
    if period.is_zero() {
        anyhow::bail!("{adapter}: period must be greater than zero");
    }
    if start_time == NanoTime::ZERO {
        anyhow::bail!(
            "{adapter}: start_time is NanoTime::ZERO; \
            use RunMode::HistoricalFrom with an explicit start time"
        );
    }
    let end_time = match end_time {
        Ok(t) if t == NanoTime::MAX => anyhow::bail!(
            "{adapter} requires RunFor::Duration; \
            RunFor::Forever would generate an unbounded number of slices"
        ),
        Ok(t) => t,
        Err(_) => anyhow::bail!(
            "{adapter} requires RunFor::Duration; \
            RunFor::Cycles does not provide an end time"
        ),
    };
    Ok(compute_time_slices(start_time, end_time, period))
}

/// Split `[start_time, end_time)` into contiguous half-open slices of length `period`.
///
/// Slices never straddle a midnight boundary: the final slice of each day clamps
/// its end to the next midnight (also a round number), and slicing resumes at
/// midnight on the following day with `iteration` reset to 0. On the first day,
/// slicing begins at the period boundary that contains `start_time` rather than
/// at midnight, so a mid-day start does not generate empty leading slices.
///
/// `start_time` must be non-zero (callers validate this) and `end_time` must be a
/// bounded time (i.e. `RunFor::Duration`, not `Forever`).
pub fn compute_time_slices(
    start_time: NanoTime,
    end_time: NanoTime,
    period: std::time::Duration,
) -> Vec<((NanoTime, NanoTime), i32, usize)> {
    const DAY_NANOS: i64 = 86_400_000_000_000;
    let period_nanos = period.as_nanos() as i64;

    let start_kdb = start_time.to_kdb_timestamp();
    let end_kdb = end_time.to_kdb_timestamp();

    let start_day = start_kdb.div_euclid(DAY_NANOS);
    // Subtract 1 before dividing so that an end_time that falls exactly on midnight
    // does not pull in an extra (empty) day. Note end_kdb can be negative for
    // pre-2000 windows (KDB epoch is 2000-01-01) — div_euclid keeps day
    // arithmetic correct there, so don't replace it with plain `/` division.
    let end_day = (end_kdb - 1).div_euclid(DAY_NANOS);

    let mut result = Vec::new();

    for day in start_day..=end_day {
        let kdb_date = day as i32;
        let midnight_kdb = day * DAY_NANOS;
        let next_midnight_kdb = midnight_kdb + DAY_NANOS;

        // For the first day, begin at the period boundary that contains start_time
        // rather than always starting at midnight.
        let mut iteration = if day == start_day {
            ((start_kdb - midnight_kdb) / period_nanos) as usize
        } else {
            0
        };

        loop {
            // Half-open intervals [t0, t1): caller uses `time >= t0, time < t1`.
            // t0 and t1 are always round multiples of period (or midnight),
            // so queries contain only clean numbers with no ±1 adjustments.
            let t0 = midnight_kdb + iteration as i64 * period_nanos;

            // On the last day, stop once the slice start has reached or passed
            // end_time — any further slice would be entirely outside the range.
            if day == end_day && t0 >= end_kdb {
                break;
            }

            let natural_t1 = t0 + period_nanos;
            // For the final slice of the day, t1 clamps to next midnight (also a round
            // number: 86400000000000j per day). For non-final slices t1 = t0 + period.
            let t1 = natural_t1.min(next_midnight_kdb);

            result.push((
                (
                    NanoTime::from_kdb_timestamp(t0),
                    NanoTime::from_kdb_timestamp(t1),
                ),
                kdb_date,
                iteration,
            ));

            if natural_t1 >= next_midnight_kdb {
                break;
            }

            iteration += 1;
        }
    }

    result
}
