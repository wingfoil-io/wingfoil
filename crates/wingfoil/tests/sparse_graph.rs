//! Phase 4.5 sparse-dispatch guard: the interpreted engine drives a dirty-list
//! seeded from the tick frontier, so per-cycle work is proportional to the
//! nodes that actually fire — not the graph size. These tests pin the two
//! properties that matters: a quiet sub-graph is *never* cycled on a tick it
//! has no part in, and the engine stays correct on a wide, sparsely-ticking
//! graph.

use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;

use wingfoil::interp::Dispatch;
use wingfoil::prelude::*;
use wingfoil::{NanoTime, RunFor, RunMode};

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
    let mut r2 = g2.build().with_dispatch(Dispatch::FullSweep);
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

/// **The Phase 4.5 perf gate.** The dirty-list's headline claim is that
/// per-cycle work is proportional to the *active* node count and not to the
/// graph size `N`. `benches/store_baseline.rs` measures that in wall-clock, but
/// nothing runs benchmarks in CI and a timing ratio is not a pass/fail signal —
/// so the claim is pinned here instead, exactly and deterministically, via
/// [`Runner::node_visits`] (nodes the dispatch loop had to look at, fired or
/// not).
///
/// The graph is one hot chain that fires every cycle, plus `cold` idle branches
/// hung off a driver whose period exceeds the whole run — so the padding
/// contributes to `N` and, after the first cycle, nothing to the active set.
///
/// The invariant is *not* that padding is free outright: every ticker is due at
/// `t = 0`, so the cold branches genuinely fire once, and counting that is
/// correct. The claim is that padding costs a **one-off**, never a per-cycle
/// tax. So the gate measures what the padding adds, `visits(wide) −
/// visits(narrow)`, at two run lengths 10x apart and asserts the two deltas are
/// *equal*: a cost that does not grow with cycles is a cost the per-cycle loop
/// does not pay. The full-sweep oracle is measured the same way as a
/// sensitivity check — its delta scales with the run length, which is what
/// proves this metric would catch a regression to an all-nodes sweep rather
/// than being trivially constant.
///
/// Scope, precisely: this pins independence from the node *count*, with shallow
/// (dangling) padding. Independence from graph *depth* is a separate property
/// with its own gate — see `quiet_depth_does_not_cost_per_cycle` below.
#[test]
fn sparse_work_is_independent_of_graph_size() {
    const HOT: Duration = Duration::from_nanos(10);
    // 1ms ≫ the simulated time these runs cover, so after the `t = 0` tick the
    // cold driver is never due again.
    const NEVER: Duration = Duration::from_millis(1);
    const NARROW: usize = 8;
    const WIDE: usize = 512;
    const SHORT: u32 = 200;
    const LONG: u32 = 2_000;

    /// One hot chain (fires every cycle) + `cold` idle branches. Returns the
    /// run's node-visit count and the hot output, so padding can be checked not
    /// to perturb results either.
    fn run_padded(cold: usize, cycles: u32, dispatch: Dispatch) -> (u64, Vec<u64>) {
        let g = GraphBuilder::new();

        let mut hot = g.ticker(HOT).count();
        for _ in 0..4 {
            hot = hot.map(|v| v.wrapping_add(1));
        }
        let out = hot.accumulate();

        // Padding: present in `N`, absent from every work set after cycle 0.
        // Handles are retained so nothing can be mistaken for dead and dropped.
        let cold_driver = g.ticker(NEVER).count();
        let _padding: Vec<_> = (0..cold)
            .map(|_| cold_driver.map(|v| v.wrapping_mul(3)).accumulate())
            .collect();

        let mut r = g.build().with_dispatch(dispatch);
        r.run(HISTORICAL, RunFor::Cycles(cycles)).unwrap();
        (r.node_visits(), r.value(&out))
    }

    /// What `WIDE − NARROW` extra cold branches cost over a run of `cycles`.
    fn padding_cost(cycles: u32, dispatch: Dispatch) -> u64 {
        let (narrow, out_narrow) = run_padded(NARROW, cycles, dispatch);
        let (wide, out_wide) = run_padded(WIDE, cycles, dispatch);
        // Padding must not perturb results either — a sanity check that the
        // wide graph really was built and really did run.
        assert_eq!(out_narrow, out_wide, "cold padding changed the hot output");
        assert!(!out_narrow.is_empty(), "hot chain produces a series");
        wide - narrow
    }

    let sparse_short = padding_cost(SHORT, Dispatch::Sparse);
    let sparse_long = padding_cost(LONG, Dispatch::Sparse);

    // The gate: the padding's cost is a constant, not a rate. Sitting quietly
    // in the graph for 10x as many cycles must not cost a quiet node anything.
    assert_eq!(
        sparse_short, sparse_long,
        "{WIDE} vs {NARROW} cold branches cost {sparse_short} extra node visits \
         over {SHORT} cycles but {sparse_long} over {LONG} — quiet nodes are \
         being charged per cycle, so per-cycle work tracks N, not the active set"
    );

    // Sensitivity: the same measurement on the retained `O(N)` oracle *must*
    // scale with the run length. Without this, the assertion above would still
    // pass if `node_visits` were accidentally counting something constant.
    let full_short = padding_cost(SHORT, Dispatch::FullSweep);
    let full_long = padding_cost(LONG, Dispatch::FullSweep);
    assert!(
        full_long > full_short * 5,
        "the full-sweep oracle should pay for every cold node every cycle \
         ({full_short} extra visits over {SHORT} cycles -> {full_long} over \
         {LONG}) — if it does not, node_visits is not measuring per-cycle \
         graph-size sensitivity"
    );
    // And the gap itself: on the long run the oracle pays orders of magnitude
    // more for the same quiet nodes — the ≪ that `Dispatch::Sparse` exists for.
    assert!(
        sparse_long * 100 < full_long,
        "sparse pays {sparse_long} for the padding over {LONG} cycles, \
         full-sweep {full_long} — expected the sparse engine to be far cheaper"
    );
}

/// **The depth half of the perf gate.** `sparse_work_is_independent_of_graph_size`
/// pins independence from the node *count*; this pins independence from graph
/// *depth*, which was a real cost until the drain stopped walking empty buckets.
///
/// The drain has to visit occupied layers in ascending order. Walking
/// `0..=max_layer` and testing each bucket makes that `O(depth)` per cycle — and
/// depth is not exotic: `fan` left-folds its branches into a binary merge chain,
/// so a 256-way fan-in is a ~256-deep graph. A mostly-quiet graph then pays for
/// its depth on every single cycle. The drain now finds occupied layers through
/// a bitmask instead, so quiet depth costs `depth/64` word tests rather than
/// `depth` bucket tests.
///
/// The shape matters, and it is the benchmark's shape rather than the obvious
/// one. Dangling quiet depth proves nothing: with nothing active above it,
/// `max_layer` stays down at the hot chain and *no* strategy ever looks at those
/// layers. The pathology needs an **active node sitting above the quiet depth** —
/// exactly what `hot.merge(&deep_quiet_chain)` produces, and exactly what `fan`
/// emits, since its merge tree makes the join layer proportional to the branch
/// count. Then `max_layer` is high on every cycle while nearly every bucket
/// under it is empty.
///
/// The metric is [`Runner::layer_visits`], which counts buckets opened *plus
/// words examined looking for them*. Counting only the buckets would be a
/// tautology: both strategies open the identical set, and differ solely in how
/// much they examine to find it.
///
/// The assertion is a ratio, not an equality. Quiet depth is not free even with
/// the bitmask — it costs `depth/64` word tests per cycle — so what is pinned is
/// that the per-cycle marginal cost of the extra depth is a *small fraction* of
/// that depth. A walk of every bucket costs ~1 per layer per cycle and misses
/// the bound by ~64x; the bitmask comes in at ~1/64 and passes with room to
/// spare. Deterministic integers throughout — the margin absorbs implementation
/// detail, not measurement noise.
#[test]
fn quiet_depth_does_not_cost_per_cycle() {
    const HOT: Duration = Duration::from_nanos(10);
    const NEVER: Duration = Duration::from_millis(1);
    const SHALLOW: usize = 2;
    const DEEP: usize = 320;
    const SHORT: u32 = 200;
    const LONG: u32 = 2_200;
    /// The drain must examine at most `1/SKIP_FACTOR` of the quiet layers it
    /// skips. Set well inside the bitmask's 64x so the gate tracks the property
    /// and not the word size, but far outside a per-bucket walk's 1x.
    const SKIP_FACTOR: u64 = 8;

    /// The same hot chain either way, joined to a quiet chain of `quiet_depth`
    /// merge/map pairs. The join is active every cycle and sits above the whole
    /// quiet chain, so `max_layer` tracks the quiet depth even though none of it
    /// fires.
    fn run_with_depth(quiet_depth: usize, cycles: u32) -> (u64, Vec<u64>) {
        let g = GraphBuilder::new();

        let mut hot = g.ticker(HOT).count();
        for _ in 0..4 {
            hot = hot.map(|v| v.wrapping_add(1));
        }

        // Quiet chain: driven by a ticker due only at `t = 0`.
        let cold = g.ticker(NEVER).count();
        let mut deep = cold.map(|v| v.wrapping_mul(3));
        for _ in 0..quiet_depth {
            deep = deep.merge(&cold).map(|v| v.wrapping_add(1));
        }

        // The join — active on every hot tick, layered above all the quiet depth.
        let out = hot.merge(&deep).accumulate();

        let mut r = g.build();
        r.run(HISTORICAL, RunFor::Cycles(cycles)).unwrap();
        (r.layer_visits(), r.value(&out))
    }

    /// What burying `DEEP - SHALLOW` quiet layers costs over a run of `cycles`.
    fn depth_cost(cycles: u32) -> u64 {
        let (shallow, shallow_out) = run_with_depth(SHALLOW, cycles);
        let (deep, deep_out) = run_with_depth(DEEP, cycles);
        assert_eq!(
            shallow_out.len(),
            deep_out.len(),
            "quiet depth changed how often the hot chain produced"
        );
        assert!(!shallow_out.is_empty(), "hot chain produces a series");
        deep - shallow
    }

    // Marginal per-cycle cost of the extra depth: the one-off `t = 0` activation
    // cancels in the difference, leaving only what each additional cycle paid.
    let extra_depth = (DEEP - SHALLOW) as u64;
    let marginal = (depth_cost(LONG) - depth_cost(SHORT)) / (LONG - SHORT) as u64;

    assert!(
        marginal * SKIP_FACTOR < extra_depth,
        "each cycle examined {marginal} layer-scan steps for {extra_depth} quiet \
         layers — expected under {} (a walk of every bucket costs ~1 per layer; \
         the occupied-layer bitmask should cost ~1/64)",
        extra_depth / SKIP_FACTOR
    );
}

/// Randomised differential parity: build many pseudo-random graphs and assert
/// the sparse engine and the retained full-sweep oracle agree on every one. A
/// single hand-wired graph (see `sparse_matches_full_sweep_oracle`) only covers
/// the shapes we thought to write; this fuzzes the two engines against each
/// other across a broad mix of schedulers, passive edges, tie-broken merges,
/// stateful folds/delays and wide fan-in, which is exactly what the oracle was
/// retained for. A deterministic LCG seeds each graph, so a failure reproduces
/// from its printed seed.
#[test]
fn sparse_matches_full_sweep_across_random_graphs() {
    // Build the *same* graph structure from a seed (the LCG is deterministic),
    // returning an accumulated series to compare. Kept to `u64` streams so every
    // combinator stays well-typed.
    fn build_random(g: &GraphBuilder, seed: u64) -> Stream<Vec<u64>> {
        let mut rng = seed;
        let mut next = || {
            // A standard 64-bit LCG (Knuth's constants); we use the high bits.
            rng = rng
                .wrapping_mul(6364136223846793005)
                .wrapping_add(1442695040888963407);
            rng >> 33
        };

        // A handful of independent schedulers at distinct periods, so islands
        // tick on different cycles — the case sparse dispatch is most about.
        let mut pool: Vec<Stream<u64>> = vec![
            g.ticker(Duration::from_nanos(1 + next() % 4)).count(),
            g.ticker(Duration::from_nanos(1 + next() % 6)).count(),
            g.ticker(Duration::from_nanos(1 + next() % 9)).count(),
        ];

        let steps = 8 + next() % 12;
        for _ in 0..steps {
            let ai = next() as usize % pool.len();
            let bi = next() as usize % pool.len();
            let k = 1 + next() % 7;
            let s = match next() % 8 {
                0 => pool[ai].map(move |v| v.wrapping_add(k)),
                1 => pool[ai].map(move |v| v.wrapping_mul(k)),
                2 => pool[ai].delay(Duration::from_nanos(1 + next() % 4)),
                3 => pool[ai].fold(0u64, |acc, v| *acc = acc.wrapping_add(*v)),
                4 => pool[ai].merge(&pool[bi]),
                5 => pool[ai].join(&pool[bi], |x, y| x.wrapping_add(*y)),
                6 => {
                    let trig = g.ticker(Duration::from_nanos(1 + next() % 3));
                    pool[ai].sample(&trig)
                }
                _ => {
                    let cond = pool[ai].map(|v| v.is_multiple_of(2));
                    pool[bi].filter(&cond)
                }
            };
            pool.push(s);
        }
        pool.last().expect("pool seeded non-empty").accumulate()
    }

    for seed in 0..64u64 {
        let g_sparse = GraphBuilder::new();
        let h_sparse = build_random(&g_sparse, seed);
        let mut r_sparse = g_sparse.build();
        r_sparse.run(HISTORICAL, RunFor::Cycles(40)).unwrap();

        let g_full = GraphBuilder::new();
        let h_full = build_random(&g_full, seed);
        let mut r_full = g_full.build().with_dispatch(Dispatch::FullSweep);
        r_full.run(HISTORICAL, RunFor::Cycles(40)).unwrap();

        assert_eq!(
            r_sparse.value(&h_sparse),
            r_full.value(&h_full),
            "sparse and full-sweep disagree on random graph seed {seed}"
        );
    }
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

/// Regression guard for the layered dispatch key: a node that reads a value
/// **passively** must still drain *after* that value updates, even when the
/// passive source sits **deeper** than the node's own active trigger.
///
/// `trigger.sample(deep_chain)` is the minimal case — the sample's only active
/// edge is a shallow trigger (layer 1), yet it passively reads a layer-4 chain.
/// A dispatch `layer` computed from *active* edges alone would sort the sample
/// (layer 1) ahead of the chain (layers 2..4) and read a **stale** value; the
/// layer must count passive edges too, matching legacy wingfoil where every
/// upstream — active or passive — raises the layer (`graph.rs:794`, and
/// `fix_layers` at `graph.rs:1153`). The random-graph oracle only hits this
/// probabilistically; this pins it deterministically.
#[test]
fn passive_read_of_deeper_node_sees_current_value() {
    // Returns (sampled series, deep series). Both tickers run at the same 1ns
    // period, so they fire on the same instants: every cycle the deep chain
    // updates *and* the trigger fires, so a correct sample sees the current
    // deep value and the two series are identical.
    fn wire(g: &GraphBuilder) -> (Stream<Vec<u64>>, Stream<Vec<u64>>) {
        let src = g.ticker(Duration::from_nanos(1)).count();
        let deep = src
            .map(|v| v.wrapping_mul(2)) // layer 2
            .map(|v| v.wrapping_add(1)) // layer 3
            .map(|v| v.wrapping_mul(10)); // layer 4
        let trigger = g.ticker(Duration::from_nanos(1));
        let sampled = deep.sample(&trigger);
        (sampled.accumulate(), deep.accumulate())
    }

    // Differential: the layered sparse engine vs the index-order full-sweep
    // oracle must agree.
    let g_sparse = GraphBuilder::new();
    let (sampled_sparse, _deep_sparse) = wire(&g_sparse);
    let mut r_sparse = g_sparse.build();
    r_sparse.run(HISTORICAL, RunFor::Cycles(20)).unwrap();

    let g_full = GraphBuilder::new();
    let (sampled_full, _deep_full) = wire(&g_full);
    let mut r_full = g_full.build().with_dispatch(Dispatch::FullSweep);
    r_full.run(HISTORICAL, RunFor::Cycles(20)).unwrap();

    assert_eq!(
        r_sparse.value(&sampled_sparse),
        r_full.value(&sampled_full),
        "layered sparse and full-sweep disagree on a passive read of a deeper node"
    );

    // Direct freshness check: the sampled series must equal the deep series —
    // i.e. the sample observed the *current* (not lagged) deep value each cycle.
    let g = GraphBuilder::new();
    let (sampled, deep) = wire(&g);
    let mut r = g.build();
    r.run(HISTORICAL, RunFor::Cycles(20)).unwrap();
    assert_eq!(
        r.value(&sampled),
        r.value(&deep),
        "sample read a stale deep value — the passive edge did not raise its layer"
    );
}
