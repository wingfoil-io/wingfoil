//! Differential parity for the statistics catalog: run the **legacy**
//! `StatisticsOperators` and their wingfoil `StatisticsOps` twins over the same
//! scenario in the same process, and compare the emitted series directly.
//!
//! The rest of the statistics suite (`statistics_rolling.rs` and friends)
//! asserts against a *hand-computed* expected series "derived from the legacy
//! node semantics". That transcription is the thing this file removes: here the
//! legacy engine computes the expectation, so a divergence is caught by the
//! oracle CLAUDE.md names rather than by whoever wrote the literal down.
//!
//! Irregularly-spaced scenarios — where the time-weighted operators stop being
//! degenerate — live in `stats_differential_irregular.rs`.

use std::cell::RefCell;
use std::rc::Rc;
use std::time::Duration;

use wingfoil::prelude::*;
use wingfoil::stats::StatisticsOps;
use wingfoil::{NanoTime, RunFor, RunMode};

use legacy_wingfoil::adapters::statistics::{StatisticsOperators, Weighting, Window};
use legacy_wingfoil::{NodeOperators, StreamOperators};

const HISTORICAL: RunMode = RunMode::HistoricalFrom(NanoTime::ZERO);
const PERIOD: Duration = Duration::from_nanos(100);
const CYCLES: u32 = 40;

/// `((n * 7) % 13)` for n = 1.., as f64 — the same non-monotonic sequence the
/// wingfoil statistics tests use.
fn legacy_series() -> Rc<dyn legacy_wingfoil::Stream<f64>> {
    legacy_wingfoil::ticker(PERIOD)
        .count()
        .map(|n: u64| ((n * 7) % 13) as f64)
}

fn wingfoil_series(g: &GraphBuilder) -> wingfoil::fluent::Stream<f64> {
    g.ticker(PERIOD)
        .count()
        .map(|n: &u64| ((*n * 7) % 13) as f64)
}

/// Run a legacy stat op and collect its emitted series.
fn legacy_run(
    build: impl Fn(&Rc<dyn legacy_wingfoil::Stream<f64>>) -> Rc<dyn legacy_wingfoil::Stream<f64>>,
) -> Vec<f64> {
    let src = legacy_series();
    let out = build(&src);
    let seen = Rc::new(RefCell::new(Vec::new()));
    let tap = {
        let seen = seen.clone();
        out.for_each(move |v, _t| seen.borrow_mut().push(v))
    };
    tap.run(HISTORICAL, RunFor::Cycles(CYCLES))
        .expect("legacy run");
    seen.borrow().clone()
}

/// Run the wingfoil twin and collect its emitted series.
fn wingfoil_run(
    build: impl Fn(&wingfoil::fluent::Stream<f64>) -> wingfoil::fluent::Stream<f64>,
) -> Vec<f64> {
    let g = GraphBuilder::new();
    let src = wingfoil_series(&g);
    let out = build(&src);
    let acc = out.accumulate();
    let mut r = g.build();
    r.run(HISTORICAL, RunFor::Cycles(CYCLES))
        .expect("wingfoil run");
    r.value(&acc)
}

fn compare(name: &str, legacy: Vec<f64>, next: Vec<f64>) {
    assert_eq!(
        legacy.len(),
        next.len(),
        "{name}: series length differs\n legacy={legacy:?}\n wingfoil={next:?}"
    );
    for (i, (l, n)) in legacy.iter().zip(next.iter()).enumerate() {
        assert!(
            (l - n).abs() < 1e-9,
            "{name}: diverges at index {i}: legacy={l} wingfoil={n}\n legacy={legacy:?}\n wingfoil={next:?}"
        );
    }
}

#[test]
fn rolling_mean_matches_legacy() {
    compare(
        "rolling_mean(5)",
        legacy_run(|s| s.mean(Window::Count(5), Weighting::Count)),
        wingfoil_run(|s| s.rolling_mean(5)),
    );
}

#[test]
fn rolling_var_matches_legacy() {
    compare(
        "rolling_var(5)",
        legacy_run(|s| s.variance(Window::Count(5), Weighting::Count)),
        wingfoil_run(|s| s.rolling_var(5)),
    );
}

#[test]
fn rolling_median_matches_legacy() {
    compare(
        "rolling_median(5)",
        legacy_run(|s| s.median(Window::Count(5), Weighting::Count)),
        wingfoil_run(|s| s.rolling_median(5)),
    );
}

#[test]
fn cumulative_var_matches_legacy() {
    compare(
        "cumulative_var",
        legacy_run(|s| s.variance(Window::Unbounded, Weighting::Count)),
        wingfoil_run(|s| s.cumulative_var()),
    );
}

#[test]
fn time_weighted_mean_matches_legacy() {
    compare(
        "cumulative_mean_time_weighted",
        legacy_run(|s| s.mean(Window::Unbounded, Weighting::Time)),
        wingfoil_run(|s| s.cumulative_mean_time_weighted()),
    );
}

#[test]
fn time_windowed_mean_matches_legacy() {
    compare(
        "time_windowed_mean(500ns)",
        legacy_run(|s| s.mean(Window::Time(Duration::from_nanos(500)), Weighting::Count)),
        wingfoil_run(|s| s.time_windowed_mean(Duration::from_nanos(500))),
    );
}
