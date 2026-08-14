//! Run mode and run bounds — how long a graph runs, and against which clock.
//!
//! Shared by both engines: the interpreted/compiled runners here and the
//! legacy engine in the `wingfoil` crate, which re-exports these types so
//! `wingfoil::RunMode` and `wingfoil::RunMode` are the *same* type and a
//! run bound can be handed across the boundary unchanged.

use std::time::Duration;

use crate::runtime::time::NanoTime;

/// Whether the graph should run in RealTime or Historical mode.
///
/// Defaults to [`RunMode::RealTime`], the production deployment mode.
#[derive(Clone, Copy, Debug, PartialEq, Default)]
pub enum RunMode {
    /// Engine time follows the wall clock, and the kernel parks between
    /// cycles. Threaded and busy-poll sources are only legal here.
    #[default]
    RealTime,
    /// Deterministic replay from the given start time: engine time is pure
    /// logic — each cycle advances to the earliest scheduled callback, never
    /// consulting a clock — so a run is reproducible cycle for cycle.
    HistoricalFrom(NanoTime),
}

impl RunMode {
    /// The engine time the run begins at: `now()` in realtime, the supplied
    /// instant in a historical replay.
    pub fn start_time(&self) -> NanoTime {
        match self {
            RunMode::RealTime => NanoTime::now(),
            RunMode::HistoricalFrom(start_time) => *start_time,
        }
    }
}

/// Defines how long the graph should run for.  Can be a
/// Duration, number of cycles or forever.
///
/// Defaults to [`RunFor::Forever`].
#[derive(Clone, Copy, Debug, Default)]
pub enum RunFor {
    /// Stop once *engine* time has advanced this far past the start time —
    /// wall-clock elapsed in realtime, replayed time in a historical run.
    Duration(Duration),
    /// Stop after this many engine cycles. Exact and deterministic in a
    /// historical replay; in realtime, burst coalescing makes the number of
    /// cycles a poor proxy for the number of values, so prefer `Duration`.
    Cycles(u32),
    /// Run until something else ends it — a source signalling end-of-stream,
    /// a `stop` handle, or an error.
    #[default]
    Forever,
}

impl RunFor {
    /// Whether the run has reached its bound, given the cycle just completed
    /// and the engine time elapsed since the start.
    pub fn done(&self, cycle: u32, elapsed: NanoTime) -> bool {
        match self {
            RunFor::Cycles(cycles) => cycle > *cycles,
            RunFor::Duration(duration) => elapsed > NanoTime::from(*duration),
            RunFor::Forever => false,
        }
    }
}
