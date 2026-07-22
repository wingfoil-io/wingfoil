//! Merge tie-break parity when **both** inputs tick in the same cycle — the
//! fable-review blind spot "merge tie-break never tested with both inputs
//! ticking in one cycle" (the existing `odds_evens` streams are disjoint by
//! construction, so the tie-break arm never runs and the compiled tick-pair
//! ordering could be swapped with the tests staying green).
//!
//! `merge2` is `if a_ticked { a } else if b_ticked { b }` — the earliest-
//! supplied ticked input wins. Two tickers of the *same* period tick on every
//! cycle, so the merge fires its tie-break arm every cycle and the first input
//! (`a`) must win. All three emission paths (interpreted, compiled, nested)
//! must agree, so a swapped tick-pair ordering in any one is caught.

use std::time::Duration;

use wingfoil::{NanoTime, RunFor, RunMode};
use wingfoil_next::prelude::*;

const HISTORICAL: RunMode = RunMode::HistoricalFrom(NanoTime::ZERO);

wingfoil_next::graph! {
    fn merge_tie(g: &GraphBuilder) -> Stream<Vec<u64>> {
        // Same period ⇒ both tick every cycle ⇒ the merge tie-break runs each
        // cycle. `a` = 1,2,3,4; `b` = 101,102,103,104.
        let a = g.ticker(Duration::from_nanos(10)).count();
        let b = g.ticker(Duration::from_nanos(10)).count().map(|c| *c + 100);
        let merged = a.merge(&b).accumulate();
        merged
    }
}

#[test]
fn merge_tie_break_first_input_wins_all_paths() {
    let run_for = RunFor::Cycles(4);

    let (mut runner, out) = merge_tie::interpreted();
    runner.run(HISTORICAL, run_for).unwrap();
    let interpreted = runner.value(out);
    // `a` wins every tie, so the merged series is a's, not b's (which would be
    // 101,102,103,104 had the ordering been swapped).
    assert_eq!(
        vec![1, 2, 3, 4],
        interpreted,
        "earliest-supplied input wins"
    );

    let (compiled,) = merge_tie::compiled(HISTORICAL, run_for).unwrap();
    assert_eq!(interpreted, compiled, "interpreted == compiled");

    let g = GraphBuilder::new();
    let island = merge_tie::nested(&g);
    let mut r = g.build();
    r.run(HISTORICAL, run_for).unwrap();
    assert_eq!(interpreted, r.value(&island), "interpreted == nested");
}
