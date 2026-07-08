//! Helpers shared across I/O adapters.
//!
//! Currently this is the out-of-window row filter used by historical/replay
//! sources. The graph clock is strictly monotonic and bounded to the run's
//! `[start_time, end_time)`: delivering a value before the clock aborts the run
//! ("received Historical message but with time less than graph time"), and can
//! even underflow `NanoTime` subtraction in debug builds. Any adapter that
//! replays a caller-parameterised range (time-sliced queries, cursor replays,
//! etc.) can over-read its bounds, so it should pass each emitted row through a
//! [`WindowFilter`] first.

use crate::time::NanoTime;

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

    /// Emit a single summary warning if any rows were discarded. Consumes `self`
    /// so it is called exactly once per batch.
    pub fn finish(self) {
        if self.dropped > 0 {
            log::warn!(
                "{}: dropped {} row(s) outside the requested window [{:?}, {:?}); \
                the source returned data beyond the range it was asked for",
                self.label,
                self.dropped,
                self.window.lo,
                self.window.hi,
            );
        }
    }
}
