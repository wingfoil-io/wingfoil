//! Run mode and run bounds — how long a graph runs, and against which clock.
//!
//! Shared by both engines: the interpreted/compiled runners here and the
//! legacy engine in the `wingfoil` crate, which re-exports these types so
//! `wingfoil::RunMode` and `wingfoil_next::RunMode` are the *same* type and a
//! run bound can be handed across the boundary unchanged.

use std::time::Duration;

use crate::runtime::time::NanoTime;

/// Whether the graph should run in RealTime or Historical mode.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum RunMode {
    RealTime,
    HistoricalFrom(NanoTime),
}

impl RunMode {
    pub fn start_time(&self) -> NanoTime {
        match self {
            RunMode::RealTime => NanoTime::now(),
            RunMode::HistoricalFrom(start_time) => *start_time,
        }
    }
}

/// Defines how long the graph should run for.  Can be a
/// Duration, number of cycles or forever.
#[derive(Clone, Copy, Debug)]
pub enum RunFor {
    Duration(Duration),
    Cycles(u32),
    Forever,
}

impl RunFor {
    pub fn done(&self, cycle: u32, elapsed: NanoTime) -> bool {
        match self {
            RunFor::Cycles(cycles) => cycle > *cycles,
            RunFor::Duration(duration) => elapsed > NanoTime::from(*duration),
            RunFor::Forever => false,
        }
    }
}
