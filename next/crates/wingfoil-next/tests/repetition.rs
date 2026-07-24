//! `map_n` / `fan` bounded-repetition sugar. Two guarantees:
//! 1. the sugar unrolls to the *same* DAG as hand-written `map`s + `merge`s;
//! 2. the dual-mode contract still holds (interpreted == compiled) for it.

use wingfoil::{NanoTime, RunFor, RunMode};

const HISTORICAL: RunMode = RunMode::HistoricalFrom(NanoTime::ZERO);

// 3 branches, each 2 `+1` maps deep, merged — via the sugar.
wingfoil_next::graph! {
    fn sugared(g: &GraphBuilder) -> Stream<u64> {
        let src = g.ticker(std::time::Duration::from_nanos(10)).count();
        let out = src.fan(3, |s| s.map_n(2, |i| *i + 1));
        out
    }
}

// The same graph, spelled out by hand.
wingfoil_next::graph! {
    fn manual(g: &GraphBuilder) -> Stream<u64> {
        let src = g.ticker(std::time::Duration::from_nanos(10)).count();
        let a = src.map(|i| *i + 1).map(|i| *i + 1);
        let b = src.map(|i| *i + 1).map(|i| *i + 1);
        let c = src.map(|i| *i + 1).map(|i| *i + 1);
        let out = a.merge(&b).merge(&c);
        out
    }
}

#[test]
fn map_n_and_fan_match_hand_unrolling() {
    let run_for = RunFor::Cycles(8);

    let (mut r_sugar, o_sugar) = sugared::interpreted();
    r_sugar.run(HISTORICAL, run_for).unwrap();
    let sugar_interp = r_sugar.value(o_sugar);
    let (sugar_compiled,) = sugared::compiled(HISTORICAL, run_for).unwrap();

    let (mut r_manual, o_manual) = manual::interpreted();
    r_manual.run(HISTORICAL, run_for).unwrap();
    let manual_interp = r_manual.value(o_manual);
    let (manual_compiled,) = manual::compiled(HISTORICAL, run_for).unwrap();

    // Dual-mode parity for the sugared and the manual graphs.
    assert_eq!(sugar_interp, sugar_compiled);
    assert_eq!(manual_interp, manual_compiled);
    // The sugar produces exactly the hand-unrolled graph's behaviour.
    assert_eq!(sugar_compiled, manual_compiled);
}
