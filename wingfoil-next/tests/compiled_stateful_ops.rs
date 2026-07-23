//! Three-engine parity for the stateful / timer / fallible catalog ops that
//! reach `graph!` purely through `#[op]` — no per-op macro table row (the old
//! `OpKind`/`OpInfo` table is gone; #496). Each op carries `#[op(build = ..)]`
//! next to its `Op` impl, which emits the naming-convention forwarders
//! (`__wf_op_<name>_*`) and the `__WF_OP_<NAME>_ACTIVATION` const the generic
//! fallback dispatches through. These tests prove `interpreted()`,
//! `compiled()`, and `nested()` (a source island in an interpreted graph) all
//! agree, exactly:
//!
//! - `throttle` / `window` — single-input timer ops (`ACTIVATION::NONE`, they
//!   read `ctx.time()`/`is_last_cycle()` but never self-schedule). `window`
//!   also exercises `#[op]`'s `start`-hook forwarding. Tick **times** are
//!   asserted via `.ticked_at()`, and the runs are sized to end on a natural
//!   flush boundary so `is_last_cycle` is a no-op — that signal is
//!   deliberately not propagated into a nested island (`Ctx::nested` hard-codes
//!   it false), so ending on a boundary keeps all three engines identical.
//! - `join3` / `try_join3` — three active input edges classified by the
//!   argument convention (`&stream` → edge). `try_join` — two edges, fallible.

use std::time::Duration;

use wingfoil::{NanoTime, RunFor, RunMode};
use wingfoil_next::prelude::*;

const HISTORICAL: RunMode = RunMode::HistoricalFrom(NanoTime::ZERO);
const PERIOD: Duration = Duration::from_nanos(10);
const INTERVAL: Duration = Duration::from_nanos(25);

/// Assert `interpreted() == compiled() == nested()` for a self-contained
/// (source) graph module whose single output is an accumulated sequence. The
/// nested check mounts the graph as a source island in an outer interpreted
/// graph, driven by the island's own ticker, and reads the island's output.
macro_rules! assert_three_engines {
    ($module:ident, $run_for:expr, $expected:expr) => {{
        let run_for = $run_for;

        let (mut runner, out) = $module::interpreted();
        runner.run(HISTORICAL, run_for).unwrap();
        let interpreted = runner.value(out);
        assert_eq!($expected, interpreted, "interpreted value mismatch");

        let (compiled,) = $module::compiled(HISTORICAL, run_for).unwrap();
        assert_eq!(interpreted, compiled, "compiled must match interpreted");

        let g = GraphBuilder::new();
        let island = $module::nested(&g);
        let mut r = g.build();
        r.run(HISTORICAL, run_for).unwrap();
        assert_eq!(
            interpreted,
            r.value(&island),
            "nested island must match interpreted"
        );
    }};
}

// --- throttle: rate-limit a per-cycle counter ------------------------------

wingfoil_next::graph! {
    fn throttle_values(g: &GraphBuilder) -> Stream<Vec<u64>> {
        let acc = g.ticker(PERIOD).count().throttle(INTERVAL).accumulate();
        acc
    }
}

wingfoil_next::graph! {
    fn throttle_times(g: &GraphBuilder) -> Stream<Vec<NanoTime>> {
        let acc = g
            .ticker(PERIOD)
            .count()
            .throttle(INTERVAL)
            .ticked_at()
            .accumulate();
        acc
    }
}

/// The 10ns counter ticks 1..7 at t = 0,10,..,60. `throttle(25ns)` emits the
/// first value (count 1 at t=0), then suppresses until 25ns have elapsed since
/// the last emit: next at t=30 (count 4), then t=60 (count 7).
#[test]
fn throttle_values_agree_across_engines() {
    assert_three_engines!(throttle_values, RunFor::Cycles(7), vec![1u64, 4, 7]);
}

/// The emission **times** for the same run: 0, 30, 60ns.
#[test]
fn throttle_times_agree_across_engines() {
    assert_three_engines!(
        throttle_times,
        RunFor::Cycles(7),
        vec![NanoTime::new(0), NanoTime::new(30), NanoTime::new(60)]
    );
}

// --- window: fixed-time-boundary buffering ---------------------------------

wingfoil_next::graph! {
    fn window_values(g: &GraphBuilder) -> Stream<Vec<Vec<u64>>> {
        let acc = g.ticker(PERIOD).count().window(INTERVAL).accumulate();
        acc
    }
}

wingfoil_next::graph! {
    fn window_times(g: &GraphBuilder) -> Stream<Vec<NanoTime>> {
        let acc = g
            .ticker(PERIOD)
            .count()
            .window(INTERVAL)
            .ticked_at()
            .accumulate();
        acc
    }
}

/// The first boundary is at t=25 (start + interval); with 10ns samples the
/// window flushes [1,2,3] at t=30, advances to t=50, and flushes [4,5] at
/// t=50. Running exactly 6 cycles (last cycle t=50) ends on that boundary, so
/// the run-end `is_last_cycle` flush is a no-op and every engine agrees —
/// including the nested island, where `is_last_cycle` is always false.
#[test]
fn window_values_agree_across_engines() {
    assert_three_engines!(
        window_values,
        RunFor::Cycles(6),
        vec![vec![1u64, 2, 3], vec![4, 5]]
    );
}

/// The window emission **times** for the same run: 30, 50ns.
#[test]
fn window_times_agree_across_engines() {
    assert_three_engines!(
        window_times,
        RunFor::Cycles(6),
        vec![NanoTime::new(30), NanoTime::new(50)]
    );
}

// --- join3 / try_join / try_join3: multi-input edges -----------------------

wingfoil_next::graph! {
    fn join3_sum(g: &GraphBuilder) -> Stream<Vec<u64>> {
        let a = g.ticker(PERIOD).count();
        let b = a.map(|i| i * 2);
        let c = a.map(|i| i * 3);
        let acc = a.join3(&b, &c, |x, y, z| x + y + z).accumulate();
        acc
    }
}

/// Three active edges (receiver + two `&stream` args). At count c the sum is
/// c + 2c + 3c = 6c → 6, 12, 18.
#[test]
fn join3_agrees_across_engines() {
    assert_three_engines!(join3_sum, RunFor::Cycles(3), vec![6u64, 12, 18]);
}

wingfoil_next::graph! {
    fn try_join_sum(g: &GraphBuilder) -> Stream<Vec<u64>> {
        let a = g.ticker(PERIOD).count();
        let b = a.map(|i| i * 10);
        let acc = a
            .try_join(&b, |x: &u64, y: &u64| Ok(x + y))
            .accumulate();
        acc
    }
}

/// Two edges, fallible closure returning `Ok`: c + 10c = 11c → 11, 22, 33.
#[test]
fn try_join_agrees_across_engines() {
    assert_three_engines!(try_join_sum, RunFor::Cycles(3), vec![11u64, 22, 33]);
}

wingfoil_next::graph! {
    fn try_join3_sum(g: &GraphBuilder) -> Stream<Vec<u64>> {
        let a = g.ticker(PERIOD).count();
        let b = a.map(|i| i * 2);
        let c = a.map(|i| i * 3);
        let acc = a
            .try_join3(&b, &c, |x: &u64, y: &u64, z: &u64| Ok(x + y + z))
            .accumulate();
        acc
    }
}

/// Three edges, fallible closure returning `Ok`: 6c → 6, 12, 18.
#[test]
fn try_join3_agrees_across_engines() {
    assert_three_engines!(try_join3_sum, RunFor::Cycles(3), vec![6u64, 12, 18]);
}

// --- fallible propagation: a returned Err aborts every engine ---------------

wingfoil_next::graph! {
    fn try_join_fails(g: &GraphBuilder) -> Stream<Vec<u64>> {
        let a = g.ticker(PERIOD).count();
        let b = a.map(|i| i * 10);
        let acc = a
            .try_join(&b, |_: &u64, _: &u64| -> anyhow::Result<u64> {
                anyhow::bail!("boom")
            })
            .accumulate();
        acc
    }
}

/// The emitted cycle threads `?`, so a returned `Err` aborts the run on both
/// standalone engines identically.
#[test]
fn try_join_error_aborts_both_engines() {
    let (mut runner, _out) = try_join_fails::interpreted();
    let interp_err = runner.run(HISTORICAL, RunFor::Cycles(3));
    assert!(interp_err.is_err(), "interpreted must abort on Err");

    let compiled_err = try_join_fails::compiled(HISTORICAL, RunFor::Cycles(3));
    assert!(compiled_err.is_err(), "compiled must abort on Err");
}
