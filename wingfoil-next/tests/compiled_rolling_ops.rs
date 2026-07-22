//! Three-engine parity for the f64 rolling-window statistics ops in the
//! `graph!` / `compiled()` path: `rolling_min`, `rolling_max`, `rolling_var`,
//! `rolling_std`, and `rolling_median`.
//!
//! Each graph is a **source island** (sources inside, no stream parameters),
//! so it emits all three expansions — `interpreted()`, `compiled()`, and
//! `nested()`. Every op is stateful (a ring-buffer / monotonic-deque / Welford
//! moment state) but purely input-activated (`Activation::NONE`), so the
//! straight-line compiled schedule represents it exactly.
//!
//! The input sequence is the fixed, non-monotonic series `[3, 1, 4, 1, 5]`
//! (mapped from `count`), with a window of 3, so min/max/median evict from both
//! ends and have unambiguous expected values. Because all three engines run the
//! *same* `Op::cycle` code in the same order, their f64 outputs are
//! bit-identical — asserted with `assert_eq!` — while the hand-computed
//! expectations are checked within a tolerance (var/std carry the usual
//! floating-point residue of incremental Welford updates).

use std::time::Duration;

use wingfoil::{NanoTime, RunFor, RunMode};
use wingfoil_next::prelude::*;

const HISTORICAL: RunMode = RunMode::HistoricalFrom(NanoTime::ZERO);
const P: Duration = Duration::from_nanos(100);
const CYCLES: RunFor = RunFor::Cycles(5);

/// Run a source-island graph through all three engines, assert they agree
/// exactly, and return the shared interpreted series.
macro_rules! three_engine {
    ($module:ident) => {{
        let (mut runner, out) = $module::interpreted();
        runner.run(HISTORICAL, CYCLES).unwrap();
        let interpreted = runner.value(out);

        let (compiled,) = $module::compiled(HISTORICAL, CYCLES).unwrap();
        assert_eq!(interpreted, compiled, "interpreted vs compiled");

        let g = GraphBuilder::new();
        let island = $module::nested(&g);
        let mut r = g.build();
        r.run(HISTORICAL, CYCLES).unwrap();
        assert_eq!(interpreted, r.value(&island), "interpreted vs nested");

        interpreted
    }};
}

/// Assert two `f64` series equal element-wise within a tolerance.
fn assert_series_approx(got: &[f64], expected: &[f64]) {
    assert_eq!(got.len(), expected.len(), "length: {got:?} vs {expected:?}");
    for (i, (g, e)) in got.iter().zip(expected).enumerate() {
        assert!(
            (g - e).abs() < 1e-10,
            "at {i}: got {g}, expected {e} (series {got:?} vs {expected:?})"
        );
    }
}

// The shared non-monotonic source `[3, 1, 4, 1, 5]` as f64, one tick per 100ns.
// (`count` is 1..=5; the closure maps each to the fixed sequence.)
macro_rules! rolling_graph {
    ($name:ident, $method:ident) => {
        wingfoil_next::graph! {
            fn $name(g: &GraphBuilder) -> Stream<Vec<f64>> {
                let acc = g
                    .ticker(P)
                    .count()
                    .map(|i| [3.0, 1.0, 4.0, 1.0, 5.0][(*i - 1) as usize])
                    .$method(3)
                    .accumulate();
                acc
            }
        }
    };
}

rolling_graph!(rolling_min_graph, rolling_min);
rolling_graph!(rolling_max_graph, rolling_max);
rolling_graph!(rolling_var_graph, rolling_var);
rolling_graph!(rolling_std_graph, rolling_std);
rolling_graph!(rolling_median_graph, rolling_median);

#[test]
fn rolling_min_three_engine_parity() {
    let v = three_engine!(rolling_min_graph);
    assert_eq!(vec![3.0, 1.0, 1.0, 1.0, 1.0], v);
}

#[test]
fn rolling_max_three_engine_parity() {
    let v = three_engine!(rolling_max_graph);
    assert_eq!(vec![3.0, 3.0, 4.0, 4.0, 5.0], v);
}

#[test]
fn rolling_median_three_engine_parity() {
    let v = three_engine!(rolling_median_graph);
    assert_eq!(vec![3.0, 2.0, 3.0, 1.0, 4.0], v);
}

#[test]
fn rolling_var_three_engine_parity() {
    let v = three_engine!(rolling_var_graph);
    // Sample variance (ddof = 1); 0.0 until two samples are present.
    assert_series_approx(&v, &[0.0, 2.0, 2.333_333_333_333, 3.0, 4.333_333_333_333]);
}

#[test]
fn rolling_std_three_engine_parity() {
    let v = three_engine!(rolling_std_graph);
    assert_series_approx(
        &v,
        &[
            0.0,
            // std of the two-sample window [3, 1] is exactly sqrt(2); using the
            // constant also sidesteps clippy::approx_constant on the literal.
            std::f64::consts::SQRT_2,
            1.527_525_231_652,
            1.732_050_807_569,
            2.081_665_999_466,
        ],
    );
}
