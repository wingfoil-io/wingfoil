//! Phase 4.5 sparse-dispatch guard: the interpreted engine drives a dirty-list
//! seeded from the tick frontier, so per-cycle work is proportional to the
//! nodes that actually fire — not the graph size. These tests pin the two
//! properties that matters: a quiet sub-graph is *never* cycled on a tick it
//! has no part in, and the engine stays correct on a wide, sparsely-ticking
//! graph.

use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;

use wingfoil::{NanoTime, RunFor, RunMode};
use wingfoil_next::interp::Dispatch;
use wingfoil_next::prelude::*;

const HISTORICAL: RunMode = RunMode::HistoricalFrom(NanoTime::ZERO);

/// The sparse dirty-list and the retained full-sweep oracle must produce
/// byte-identical results. Runs one non-trivial graph — scheduling (ticker,
/// delay), a passive edge (`sample` of a delayed slot, the case most sensitive
/// to evaluation order), a filter, and a tie-broken `merge` — under both
/// dispatch strategies and asserts they agree.
#[test]
fn sparse_matches_full_sweep_oracle() {
    fn wire(g: &GraphBuilder) -> Stream<Vec<u64>> {
        let n = g.ticker(Duration::from_nanos(10)).count();
        let keep_even = n.map(|i| i.is_multiple_of(2));
        let evens = n.filter(&keep_even);
        let delayed = n.delay(Duration::from_nanos(30));
        let trig = g.ticker(Duration::from_nanos(10));
        let sampled = delayed.sample(&trig);
        sampled.merge(&evens).accumulate()
    }

    let g1 = GraphBuilder::new();
    let h1 = wire(&g1);
    let mut r1 = g1.build();
    r1.run(HISTORICAL, RunFor::Cycles(15)).unwrap();
    let sparse = r1.value(&h1);

    let g2 = GraphBuilder::new();
    let h2 = wire(&g2);
    let mut r2 = g2.build();
    r2.with_dispatch(Dispatch::FullSweep);
    r2.run(HISTORICAL, RunFor::Cycles(15)).unwrap();
    let full_sweep = r2.value(&h2);

    assert!(!sparse.is_empty(), "graph produces a series");
    assert_eq!(
        sparse, full_sweep,
        "sparse dispatch matches the full-sweep oracle"
    );
}

/// A wide graph: one *fast* driver feeds a tiny active chain that ticks every
/// cycle; a bank of `IDLE_WIDTH` maps hangs off a *slow* driver that fires far
/// less often. Each op counts its own `cycle` invocations. The sparse engine
/// must cycle an idle map only on the cycles its slow upstream actually ticked
/// — so the idle bank's total work equals `IDLE_WIDTH × slow_fires`, a handful,
/// never `IDLE_WIDTH × cycles`. A regression to an all-nodes sweep (or an
/// over-eager propagation) would fire the idle bank on the fast-only cycles and
/// blow this count up.
#[test]
fn quiet_subgraph_is_not_cycled_on_unrelated_ticks() {
    let g = GraphBuilder::new();

    let active_hits = Arc::new(AtomicUsize::new(0));
    let idle_hits = Arc::new(AtomicUsize::new(0));

    // Fast driver: 1,2,3,… once per cycle (period 10ns).
    let fast_count = g.ticker(Duration::from_nanos(10)).count();
    let ah = active_hits.clone();
    let active = fast_count
        .map(move |v| {
            ah.fetch_add(1, Ordering::Relaxed);
            *v
        })
        .accumulate();

    // Slow driver: an order of magnitude quieter (period 1000ns).
    let slow_count = g.ticker(Duration::from_nanos(1000)).count();
    const IDLE_WIDTH: usize = 500;
    let mut idle = Vec::with_capacity(IDLE_WIDTH);
    for _ in 0..IDLE_WIDTH {
        let ih = idle_hits.clone();
        idle.push(slow_count.map(move |v| {
            ih.fetch_add(1, Ordering::Relaxed);
            *v
        }));
    }

    let mut r = g.build();
    // Long enough that the slow driver fires at least once while the fast one
    // fires ~150 times.
    r.run(HISTORICAL, RunFor::Cycles(150)).unwrap();

    let fast_fires = r.value(&fast_count) as usize;
    let slow_fires = r.value(&slow_count) as usize;
    let active_work = active_hits.load(Ordering::Relaxed);
    let idle_work = idle_hits.load(Ordering::Relaxed);

    // The active chain accumulated every fast value, in order.
    assert_eq!(
        r.value(&active),
        (1..=fast_fires as u64).collect::<Vec<_>>(),
        "active chain sees every fast tick"
    );
    // The active chain fires exactly once per fast tick.
    assert_eq!(active_work, fast_fires, "active chain fires per fast tick");
    // The idle bank fires exactly once per idle map per slow tick — and the
    // slow driver genuinely ticked, so this exercises the idle path too.
    assert!(slow_fires >= 1, "slow driver should fire at least once");
    assert_eq!(
        idle_work,
        IDLE_WIDTH * slow_fires,
        "idle bank cycles only on slow ticks, never on fast-only cycles"
    );
    // The whole point: the 500-wide idle bank was NOT swept every cycle.
    assert!(slow_fires < fast_fires, "slow driver is quieter than fast");
    assert!(
        idle_work < IDLE_WIDTH * fast_fires,
        "sparse: idle work {idle_work} ≪ dense sweep {}",
        IDLE_WIDTH * fast_fires
    );
}

/// Correctness at scale: a wide fan-out where every branch is a distinct
/// arithmetic transform of one shared counter. The engine must fire each branch
/// once per cycle after the shared source, glitch-free, and land the right
/// value in every slot.
#[test]
fn wide_fanout_is_correct() {
    let g = GraphBuilder::new();
    let count = g.ticker(Duration::from_nanos(10)).count();

    const WIDTH: usize = 300;
    let mut branches = Vec::with_capacity(WIDTH);
    for k in 0..WIDTH {
        let k = k as u64;
        branches.push(count.map(move |v| *v * 1000 + k));
    }

    let mut r = g.build();
    r.run(HISTORICAL, RunFor::Cycles(20)).unwrap();

    // After 20 cycles the shared count is 20; every branch reflects it.
    let count_final = r.value(&count);
    assert_eq!(count_final, 20);
    for (k, b) in branches.iter().enumerate() {
        assert_eq!(
            r.value(b),
            count_final * 1000 + k as u64,
            "branch {k} holds its own transform of the shared count"
        );
    }
}
