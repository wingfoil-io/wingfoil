//! Differential parity for the statistics catalog under the scenarios the
//! hand-written suite does *not* cover: **irregular tick spacing** — where the
//! time-weighted and time-windowed operators stop being degenerate — plus
//! even-sized median windows and window sizes at the edges (1, and larger than
//! the series).
//!
//! Every existing statistics test drives a uniform ticker, which makes a
//! time-weighted mean agree with a count-weighted one for the wrong reason.
//! Here the source only ticks when `n % 3 != 0`, so the gaps alternate
//! 100ns / 200ns and the weighting actually has to be right.
//!
//! See `stats_differential.rs` for the regularly-spaced cases and for why the
//! comparison runs against the legacy engine rather than a transcribed
//! expectation.

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

/// An **irregularly spaced** f64 series: values arrive on a 100ns ticker but
/// only on ticks where `n % 3 != 0`, so the gaps alternate 100ns / 200ns. Every
/// time-weighted and time-windowed operator should notice; a count-weighted one
/// should not.
fn legacy_irregular() -> Rc<dyn legacy_wingfoil::Stream<f64>> {
    legacy_wingfoil::ticker(PERIOD)
        .count()
        .filter_value(|n: &u64| !n.is_multiple_of(3))
        .map(|n: u64| ((n * 7) % 13) as f64)
}

fn wingfoil_irregular(g: &GraphBuilder) -> wingfoil::fluent::Stream<f64> {
    g.ticker(PERIOD)
        .count()
        .map_filter(|n: &u64| (((*n * 7) % 13) as f64, !(*n).is_multiple_of(3)))
}

fn legacy_run(
    build: impl Fn(&Rc<dyn legacy_wingfoil::Stream<f64>>) -> Rc<dyn legacy_wingfoil::Stream<f64>>,
) -> Vec<f64> {
    let out = build(&legacy_irregular());
    let seen = Rc::new(RefCell::new(Vec::new()));
    let tap = {
        let seen = seen.clone();
        out.for_each(move |v, _t| seen.borrow_mut().push(v))
    };
    tap.run(HISTORICAL, RunFor::Cycles(CYCLES))
        .expect("legacy run");
    seen.borrow().clone()
}

fn wingfoil_run(
    build: impl Fn(&wingfoil::fluent::Stream<f64>) -> wingfoil::fluent::Stream<f64>,
) -> Vec<f64> {
    let g = GraphBuilder::new();
    let out = build(&wingfoil_irregular(&g));
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
        "{name}: series LENGTH differs — legacy emitted {} values, wingfoil {}\n legacy={legacy:?}\n wingfoil={next:?}",
        legacy.len(),
        next.len()
    );
    for (i, (l, n)) in legacy.iter().zip(next.iter()).enumerate() {
        assert!(
            (l - n).abs() < 1e-9,
            "{name}: diverges at index {i}: legacy={l} wingfoil={n}\n legacy={legacy:?}\n wingfoil={next:?}"
        );
    }
}

#[test]
fn irregular_time_weighted_mean() {
    compare(
        "cumulative_mean_time_weighted (irregular)",
        legacy_run(|s| s.mean(Window::Unbounded, Weighting::Time)),
        wingfoil_run(|s| s.cumulative_mean_time_weighted()),
    );
}

#[test]
fn irregular_time_weighted_var() {
    compare(
        "cumulative_var_time_weighted (irregular)",
        legacy_run(|s| s.variance(Window::Unbounded, Weighting::Time)),
        wingfoil_run(|s| s.cumulative_var_time_weighted()),
    );
}

#[test]
fn irregular_time_windowed_mean() {
    compare(
        "time_windowed_mean(500ns) (irregular)",
        legacy_run(|s| s.mean(Window::Time(Duration::from_nanos(500)), Weighting::Count)),
        wingfoil_run(|s| s.time_windowed_mean(Duration::from_nanos(500))),
    );
}

#[test]
fn irregular_time_windowed_var() {
    compare(
        "time_windowed_var(500ns) (irregular)",
        legacy_run(|s| s.variance(Window::Time(Duration::from_nanos(500)), Weighting::Count)),
        wingfoil_run(|s| s.time_windowed_var(Duration::from_nanos(500))),
    );
}

#[test]
fn irregular_time_windowed_median() {
    compare(
        "time_windowed_median(500ns) (irregular)",
        legacy_run(|s| s.median(Window::Time(Duration::from_nanos(500)), Weighting::Count)),
        wingfoil_run(|s| s.time_windowed_median(Duration::from_nanos(500))),
    );
}

#[test]
fn irregular_time_weighted_median() {
    compare(
        "cumulative_median_time_weighted (irregular)",
        legacy_run(|s| s.median(Window::Unbounded, Weighting::Time)),
        wingfoil_run(|s| s.cumulative_median_time_weighted()),
    );
}

#[test]
fn even_median_window() {
    compare(
        "rolling_median(4) (irregular, even window)",
        legacy_run(|s| s.median(Window::Count(4), Weighting::Count)),
        wingfoil_run(|s| s.rolling_median(4)),
    );
}

#[test]
fn window_of_one() {
    compare(
        "rolling_var(1)",
        legacy_run(|s| s.variance(Window::Count(1), Weighting::Count)),
        wingfoil_run(|s| s.rolling_var(1)),
    );
}

#[test]
fn window_larger_than_series() {
    compare(
        "rolling_var(1000)",
        legacy_run(|s| s.variance(Window::Count(1000), Weighting::Count)),
        wingfoil_run(|s| s.rolling_var(1000)),
    );
}

#[test]
fn irregular_time_weighted_count_window_mean() {
    compare(
        "mean(Count(5), Time) (irregular)",
        legacy_run(|s| s.mean(Window::Count(5), Weighting::Time)),
        wingfoil_run(|s| s.rolling_mean_time_weighted(5)),
    );
}

#[test]
fn irregular_time_weighted_count_window_var() {
    compare(
        "variance(Count(5), Time) (irregular)",
        legacy_run(|s| s.variance(Window::Count(5), Weighting::Time)),
        wingfoil_run(|s| s.rolling_var_time_weighted(5)),
    );
}
