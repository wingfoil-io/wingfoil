//! Three-engine parity for the stateless / single-input catalog ops brought
//! into the `graph!` / `compiled()` path: `distinct`, `difference`, `limit`,
//! `inspect`, `map_filter`, `with_time`, `ticked_at`, and
//! `ticked_at_elapsed`.
//!
//! Every graph here is a **source island** (sources inside, no stream
//! parameters), so it emits all three expansions — `interpreted()`,
//! `compiled()`, and `nested()`. Each test asserts the three agree exactly;
//! since they are derived from the same tokens, any drift (a wrong dispatch
//! flag, a mis-seeded value slot, a re-evaluated closure config) shows up as
//! an inequality here.

use std::time::Duration;

use wingfoil::{NanoTime, RunFor, RunMode};
use wingfoil_next::prelude::*;

const HISTORICAL: RunMode = RunMode::HistoricalFrom(NanoTime::ZERO);
const P: Duration = Duration::from_nanos(100);

/// Run a source-island graph through all three engines and assert they agree,
/// returning the (shared) interpreted value for any additional exact checks.
macro_rules! three_engine {
    ($module:ident, $run_for:expr) => {{
        let run_for = $run_for;

        let (mut runner, out) = $module::interpreted();
        runner.run(HISTORICAL, run_for).unwrap();
        let interpreted = runner.value(out);

        let (compiled,) = $module::compiled(HISTORICAL, run_for).unwrap();
        assert_eq!(interpreted, compiled, "interpreted vs compiled");

        let g = GraphBuilder::new();
        let island = $module::nested(&g);
        let mut r = g.build();
        r.run(HISTORICAL, run_for).unwrap();
        assert_eq!(interpreted, r.value(&island), "interpreted vs nested");

        interpreted
    }};
}

// ---------------------------------------------------------------------------
// distinct: suppress consecutive duplicates.
// ---------------------------------------------------------------------------
wingfoil_next::graph! {
    fn distinct_graph(g: &GraphBuilder) -> Stream<Vec<u64>> {
        let acc = g.ticker(P).count().map(|i| i / 2).distinct().accumulate();
        acc
    }
}

#[test]
fn distinct_three_engine_parity() {
    // count = 1,2,3,4,5,6 -> /2 = 0,1,1,2,2,3 -> distinct = 0,1,2,3.
    let v = three_engine!(distinct_graph, RunFor::Cycles(6));
    assert_eq!(vec![0, 1, 2, 3], v);
}

// ---------------------------------------------------------------------------
// difference: successive value - previous (quiet on the first value).
// ---------------------------------------------------------------------------
wingfoil_next::graph! {
    fn difference_graph(g: &GraphBuilder) -> Stream<Vec<i64>> {
        let acc = g
            .ticker(P)
            .count()
            .map(|i| (*i * *i) as i64)
            .difference()
            .accumulate();
        acc
    }
}

#[test]
fn difference_three_engine_parity() {
    // squares of 1,2,3,4 = 1,4,9,16 -> differences = 3,5,7 (first is quiet).
    let v = three_engine!(difference_graph, RunFor::Cycles(4));
    assert_eq!(vec![3, 5, 7], v);
}

// ---------------------------------------------------------------------------
// limit: pass the first N values, then stay quiet.
// ---------------------------------------------------------------------------
wingfoil_next::graph! {
    fn limit_graph(g: &GraphBuilder) -> Stream<Vec<u64>> {
        let acc = g.ticker(P).count().limit(3).accumulate();
        acc
    }
}

#[test]
fn limit_three_engine_parity() {
    // count would be 1..=6, but limit(3) passes only the first three.
    let v = three_engine!(limit_graph, RunFor::Cycles(6));
    assert_eq!(vec![1, 2, 3], v);
}

// ---------------------------------------------------------------------------
// inspect: side-effecting tap that passes its value through unchanged. The
// observer closure is a `Cfg` type parameter (like `map`), so this also checks
// a non-mutating closure config flows through the compiled emission.
// ---------------------------------------------------------------------------
wingfoil_next::graph! {
    fn inspect_graph(g: &GraphBuilder) -> Stream<Vec<u64>> {
        let acc = g
            .ticker(P)
            .count()
            .inspect(|i| {
                let _ = i;
            })
            .map(|i| i * 3)
            .accumulate();
        acc
    }
}

#[test]
fn inspect_three_engine_parity() {
    let v = three_engine!(inspect_graph, RunFor::Cycles(3));
    assert_eq!(vec![3, 6, 9], v);
}

// ---------------------------------------------------------------------------
// map_filter: map + filter in one pass; the closure returns `(B, bool)`.
// ---------------------------------------------------------------------------
wingfoil_next::graph! {
    fn map_filter_graph(g: &GraphBuilder) -> Stream<Vec<u64>> {
        let acc = g
            .ticker(P)
            .count()
            .map_filter(|i| (i * 10, i.is_multiple_of(2)))
            .accumulate();
        acc
    }
}

#[test]
fn map_filter_three_engine_parity() {
    // count 1..=6; emit `i*10` only when `i` is even -> 20, 40, 60.
    let v = three_engine!(map_filter_graph, RunFor::Cycles(6));
    assert_eq!(vec![20, 40, 60], v);
}

// ---------------------------------------------------------------------------
// with_time: pair each value with the current engine time.
// ---------------------------------------------------------------------------
wingfoil_next::graph! {
    fn with_time_graph(g: &GraphBuilder) -> Stream<Vec<(NanoTime, u64)>> {
        let acc = g.ticker(P).count().with_time().accumulate();
        acc
    }
}

#[test]
fn with_time_three_engine_parity() {
    // ticker fires at 0, 100, 200; counts 1, 2, 3.
    let v = three_engine!(with_time_graph, RunFor::Cycles(3));
    assert_eq!(
        vec![
            (NanoTime::from(0u64), 1),
            (NanoTime::from(100u64), 2),
            (NanoTime::from(200u64), 3),
        ],
        v
    );
}

// ---------------------------------------------------------------------------
// with_time seeding: a `with_time` fed by a `fold` with a *non-default* init,
// read via a passive `sample` edge *before* it first ticks. This is the case
// that would catch seeding drift: interp seeds the slot with
// `(ZERO, fold_init)`, so a `Default`-seeded compiled slot (`(ZERO, 0)`) would
// disagree on the pre-first-tick reads. The `fold` only fires once the gate
// opens (count > 2), so the sample at t=0 and t=100 must read the seed.
// ---------------------------------------------------------------------------
wingfoil_next::graph! {
    fn with_time_seed_graph(g: &GraphBuilder) -> Stream<Vec<(NanoTime, u64)>> {
        let count = g.ticker(P).count();
        let gate = count.map(|i| *i > 2);
        let gated = count.filter(&gate);
        let wt = gated.fold(100u64, |acc, v| *acc += v).with_time();
        let trigger = g.ticker(P);
        let acc = wt.sample(&trigger).accumulate();
        acc
    }
}

#[test]
fn with_time_seed_read_before_first_tick_parity() {
    // count 1..=4 at t = 0,100,200,300. gate opens at count 3 (t=200), so
    // `gated` ticks at 200 (v=3) and 300 (v=4); the fold seeds at 100.
    //   t=0   : fold quiet, wt = seed (ZERO, 100); sample -> (0, 100)
    //   t=100 : fold quiet, wt = seed (ZERO, 100); sample -> (0, 100)
    //   t=200 : fold 100+3=103, wt = (200, 103); sample -> (200, 103)
    //   t=300 : fold 103+4=107, wt = (300, 107); sample -> (300, 107)
    let v = three_engine!(with_time_seed_graph, RunFor::Cycles(4));
    assert_eq!(
        vec![
            (NanoTime::from(0u64), 100),
            (NanoTime::from(0u64), 100),
            (NanoTime::from(200u64), 103),
            (NanoTime::from(300u64), 107),
        ],
        v
    );
}

// ---------------------------------------------------------------------------
// ticked_at: emit the engine time whenever the upstream ticks.
// ---------------------------------------------------------------------------
wingfoil_next::graph! {
    fn ticked_at_graph(g: &GraphBuilder) -> Stream<Vec<NanoTime>> {
        let acc = g.ticker(P).count().ticked_at().accumulate();
        acc
    }
}

#[test]
fn ticked_at_three_engine_parity() {
    let v = three_engine!(ticked_at_graph, RunFor::Cycles(3));
    assert_eq!(
        vec![
            NanoTime::from(0u64),
            NanoTime::from(100u64),
            NanoTime::from(200u64),
        ],
        v
    );
}

// ---------------------------------------------------------------------------
// ticked_at_elapsed: emit elapsed engine time (now - start) on each tick.
// ---------------------------------------------------------------------------
wingfoil_next::graph! {
    fn ticked_at_elapsed_graph(g: &GraphBuilder) -> Stream<Vec<NanoTime>> {
        let acc = g.ticker(P).count().ticked_at_elapsed().accumulate();
        acc
    }
}

#[test]
fn ticked_at_elapsed_three_engine_parity() {
    // start time is ZERO, so elapsed == absolute here: 0, 100, 200.
    let v = three_engine!(ticked_at_elapsed_graph, RunFor::Cycles(3));
    assert_eq!(
        vec![
            NanoTime::from(0u64),
            NanoTime::from(100u64),
            NanoTime::from(200u64),
        ],
        v
    );
}
