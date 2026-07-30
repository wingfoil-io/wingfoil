//! Shared helpers for the adapter bindings — the run-shape plumbing every
//! source binding needs.
//!
//! A Rust caller picks the run mode at `run()`, but several adapters need it (or
//! the whole run window) at **wiring** time: a time-sliced reader slices its
//! queries up front, and a live-tail source rejects a historical run. A Python
//! `Graph` does not know its run mode until `run()` either, so those bindings
//! take the run shape as arguments and rebuild it here — which is the same three
//! conversions in every one of them.

use anyhow::{Result, bail};
use std::time::Duration;
use wingfoil::{NanoTime, RunFor, RunMode};
use wingfoil_next::async_source::RunParams;

/// The [`RunParams`] a **historical** source is wired for, from the Python
/// `start_nanos` / `duration_nanos` arguments.
///
/// `RunFor::Duration` is the only bound a time-sliced reader can slice (it needs
/// a concrete end time), so this always builds one. The values must match the
/// eventual `graph.run(start_nanos=…, duration_nanos=…)`; a reader validates the
/// window at wiring, so a mismatched or empty one is rejected there.
pub fn historical_params(start_nanos: u64, duration_nanos: u64) -> RunParams {
    let start = NanoTime::from(start_nanos);
    RunParams {
        run_mode: RunMode::HistoricalFrom(start),
        run_for: RunFor::Duration(Duration::from_nanos(duration_nanos)),
        start_time: start,
    }
}

/// The [`RunParams`] a **real-time** source is wired for. Unbounded: a live
/// source's bound comes from the actual `run()`, not from wiring.
pub fn realtime_params() -> RunParams {
    RunParams {
        run_mode: RunMode::RealTime,
        run_for: RunFor::Forever,
        start_time: NanoTime::ZERO,
    }
}

/// The [`RunMode`] a source is being wired for, from the Python `realtime` flag.
///
/// The historical arm carries `ZERO`: a source taking only a mode (rather than
/// full params) uses it to *reject* the wrong mode, never to read the instant.
pub fn run_mode(realtime: bool) -> RunMode {
    if realtime {
        RunMode::RealTime
    } else {
        RunMode::HistoricalFrom(NanoTime::ZERO)
    }
}

/// Unix seconds -> [`NanoTime`], for the cursor/offset arguments adapters take
/// as a Python `float`. Rejects a negative or non-finite input rather than
/// wrapping it into a nonsense instant.
pub fn secs_to_nanotime(secs: f64) -> Result<NanoTime> {
    if !secs.is_finite() || secs < 0.0 {
        bail!("expected a finite, non-negative Unix-seconds value, got {secs}");
    }
    Ok(NanoTime::new((secs * 1e9) as u64))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn historical_params_span_the_requested_window() {
        let params = historical_params(1_000, 500);
        assert!(matches!(
            params.run_mode,
            RunMode::HistoricalFrom(t) if t == NanoTime::from(1_000_u64)
        ));
        assert!(matches!(params.run_for, RunFor::Duration(d) if d.as_nanos() == 500));
        assert_eq!(NanoTime::from(1_000_u64), params.start_time);
    }

    #[test]
    fn realtime_params_are_unbounded() {
        let params = realtime_params();
        assert!(matches!(params.run_mode, RunMode::RealTime));
        assert!(matches!(params.run_for, RunFor::Forever));
    }

    #[test]
    fn run_mode_maps_the_realtime_flag() {
        assert!(matches!(run_mode(true), RunMode::RealTime));
        assert!(matches!(run_mode(false), RunMode::HistoricalFrom(_)));
    }

    #[test]
    fn secs_to_nanotime_rejects_nonsense() {
        assert!(secs_to_nanotime(-1.0).is_err());
        assert!(secs_to_nanotime(f64::NAN).is_err());
        assert!(secs_to_nanotime(f64::INFINITY).is_err());
        assert_eq!(NanoTime::new(1_500_000_000), secs_to_nanotime(1.5).unwrap());
        assert_eq!(NanoTime::ZERO, secs_to_nanotime(0.0).unwrap());
    }
}
