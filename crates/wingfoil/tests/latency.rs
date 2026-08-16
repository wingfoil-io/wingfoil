//! Latency capture parity tests — ports the node-layer cases from legacy
//! `wingfoil::latency` to the wingfoil Op engine. The pure data layer
//! (`Traced`/`Latency`/`Stage`/`StageStats`/`LatencyStats` layout + arithmetic)
//! is reused verbatim from the legacy crate and is covered by its own unit
//! tests; here we assert the *engine* behaviour: that `stamp`/`stamp_precise`
//! write wall-clock time (shared per cycle, fresh for `_precise`) and that
//! `latency_report` aggregates per-stage deltas.
//!
//! Sources are synthesised with `ticker().count().map(..)` — the delivery time
//! is irrelevant here (latency records ride inside the payload and stamps read
//! the wall clock), so this avoids the burst-grouped `channel` source and its
//! producer threads while exercising the identical stamping / aggregation path.

use std::time::Duration;

use wingfoil::latency::*;
use wingfoil::prelude::*;
use wingfoil::{NanoTime, RunFor, RunMode};

latency_stages! {
    pub TradeLatency {
        ingest,
        decode,
        strategy,
        publish,
    }
}

/// A source of `count` fully-defaulted `Traced<u64, TradeLatency>` messages
/// (payload = the 1-based tick index), with no latency stages set.
fn traced_source(g: &GraphBuilder) -> Stream<Traced<u64, TradeLatency>> {
    g.ticker(Duration::from_millis(1))
        .count()
        .map(|n: &u64| Traced::<u64, TradeLatency>::new(*n))
}

// ── Re-export / macro-in-wingfoil sanity ────────────────────────────────────────
// The derive is engine-agnostic and re-exported unchanged; these confirm the
// macro expands and the traits resolve inside the wingfoil crate.

#[test]
fn latency_stages_derive_works_in_next() {
    assert_eq!(TradeLatency::N, 4);
    assert_eq!(
        TradeLatency::stage_names(),
        &["ingest", "decode", "strategy", "publish"]
    );
    let l = TradeLatency {
        ingest: 1,
        decode: 2,
        strategy: 3,
        publish: 4,
    };
    assert_eq!(l.stamps(), &[1u64, 2, 3, 4]);
    assert_eq!(<trade_latency::strategy as Stage<TradeLatency>>::INDEX, 2);
}

#[test]
fn has_latency_round_trip() {
    let mut t: Traced<u64, TradeLatency> = Traced::new(7);
    t.latency_mut().strategy = 42;
    assert_eq!(t.latency().strategy, 42);
    assert_eq!(t.payload, 7);
}

// ── stamp ───────────────────────────────────────────────────────────────────

/// Legacy `stamp_stream_writes_wall_time_into_named_stage`: stamps use
/// wall-clock time, so in historical mode we assert monotonicity, not exact
/// values. Untouched stages stay zero; the payload passes through.
#[test]
fn stamp_writes_wall_time_into_named_stage() {
    let g = GraphBuilder::new();
    let acc = traced_source(&g)
        .stamp::<trade_latency::strategy>()
        .accumulate();
    let mut r = g.build();
    r.run(RunMode::HistoricalFrom(NanoTime::ZERO), RunFor::Cycles(2))
        .unwrap();

    let collected = r.value(&acc);
    assert_eq!(collected.len(), 2);
    assert_eq!(collected[0].payload, 1);
    assert_eq!(collected[1].payload, 2);
    // Untouched stages remain zero.
    assert_eq!(collected[0].latency.ingest, 0);
    assert_eq!(collected[1].latency.ingest, 0);
    // Stamps are real wall-clock times: both non-zero, second >= first.
    assert!(collected[0].latency.strategy > 0);
    assert!(collected[1].latency.strategy >= collected[0].latency.strategy);
}

/// Legacy `stamp_works_identically_in_historical_and_realtime`: same wiring,
/// both run modes produce non-zero, monotonic wall-clock stamps.
#[test]
fn stamp_works_identically_in_historical_and_realtime() {
    fn run_one(mode: RunMode) -> NanoTime {
        let g = GraphBuilder::new();
        let acc = traced_source(&g)
            .stamp::<trade_latency::ingest>()
            .stamp_precise::<trade_latency::publish>()
            .accumulate();
        let mut r = g.build();
        r.run(mode, RunFor::Cycles(3)).unwrap();
        let values = r.value(&acc);
        assert!(!values.is_empty());
        let l = values[0].latency;
        assert!(l.ingest > 0, "ingest stamp should be populated");
        assert!(l.publish >= l.ingest, "publish >= ingest");
        NanoTime::new(l.ingest)
    }
    let historical = run_one(RunMode::HistoricalFrom(NanoTime::ZERO));
    let realtime = run_one(RunMode::RealTime);
    // Both modes stamp wall-clock nanos-since-epoch.
    assert!(u64::from(historical) > 1_000_000_000);
    assert!(u64::from(realtime) > 1_000_000_000);
}

/// Legacy `stamp_if_disabled_inserts_no_node`: a disabled stamp is identity —
/// the stage it would have written stays zero.
///
/// Legacy (and wingfoil until the `Stamping` rework) spelled this
/// `stamp_if(false)`. That method is gone; `Stamping::on_if(false)` is its
/// literal translation and this test is pinned to that spelling, so the
/// recorded *behaviour* survives the removal of the surface that expressed it.
#[test]
fn stamp_disabled_is_identity() {
    let g = GraphBuilder::new();
    let acc = traced_source(&g)
        .stamp_as::<trade_latency::ingest>(Stamping::on_if(false))
        .accumulate();
    let mut r = g.build();
    r.run(RunMode::HistoricalFrom(NanoTime::ZERO), RunFor::Cycles(1))
        .unwrap();

    let collected = r.value(&acc);
    assert_eq!(collected.len(), 1);
    assert_eq!(collected[0].payload, 1);
    // Disabled: no stamp written.
    assert_eq!(collected[0].latency.ingest, 0);
}

/// Legacy `stamp_precise_writes_fresh_timestamps`: two `stamp_precise`
/// wrappers in series both produce non-zero, ordered stamps.
#[test]
fn stamp_precise_writes_fresh_timestamps() {
    let g = GraphBuilder::new();
    let acc = traced_source(&g)
        .stamp_precise::<trade_latency::ingest>()
        .stamp_precise::<trade_latency::publish>()
        .accumulate();
    let mut r = g.build();
    r.run(RunMode::HistoricalFrom(NanoTime::ZERO), RunFor::Cycles(1))
        .unwrap();

    let collected = r.value(&acc);
    assert_eq!(collected.len(), 1);
    let l = collected[0].latency;
    assert!(l.ingest > 0);
    assert!(l.publish >= l.ingest);
}

/// Legacy `multiple_stamps_compose`: three cached-`stamp` wrappers in the same
/// engine cycle share the cycle-start wall snap — the key check that
/// `Ctx::wall_time` is snapped once per cycle (the Kernel change).
#[test]
fn multiple_stamps_compose() {
    let g = GraphBuilder::new();
    let acc = traced_source(&g)
        .stamp::<trade_latency::ingest>()
        .stamp::<trade_latency::strategy>()
        .stamp::<trade_latency::publish>()
        .accumulate();
    let mut r = g.build();
    r.run(RunMode::HistoricalFrom(NanoTime::ZERO), RunFor::Cycles(1))
        .unwrap();

    let collected = r.value(&acc);
    assert_eq!(collected.len(), 1);
    let l = collected[0].latency;
    // All three run in one engine cycle and share the cycle-start snap.
    assert!(l.ingest > 0);
    assert_eq!(l.strategy, l.ingest);
    assert_eq!(l.publish, l.ingest);
    assert_eq!(l.decode, 0);
}

// ── latency_report ──────────────────────────────────────────────────────────

/// Legacy `latency_report_aggregates_across_ticks`: three fully-stamped
/// messages give exact per-stage delta means. The latency records are set on
/// the payload (deltas 10 / 20 / 30 ns), so aggregation is deterministic.
#[test]
fn latency_report_aggregates_across_ticks() {
    let g = GraphBuilder::new();
    let source = g.ticker(Duration::from_millis(1)).count().map(|n: &u64| {
        let base = n * 100; // 100, 200, 300
        Traced::with_latency(
            *n,
            TradeLatency {
                ingest: base,
                decode: base + 10,
                strategy: base + 30,
                publish: base + 60,
            },
        )
    });
    let (_sink, stats) = source.latency_report(ReportOutput::Silent);
    let mut r = g.build();
    r.run(RunMode::HistoricalFrom(NanoTime::ZERO), RunFor::Cycles(3))
        .unwrap();

    let s = stats.borrow();
    // 3 messages, each contributes one delta per non-zero stage transition.
    assert_eq!(s.stages[1].count, 3); // ingest → decode (10ns each)
    assert_eq!(s.stages[1].mean_ns(), 10);
    assert_eq!(s.stages[2].count, 3); // decode → strategy (20ns each)
    assert_eq!(s.stages[2].mean_ns(), 20);
    assert_eq!(s.stages[3].count, 3); // strategy → publish (30ns each)
    assert_eq!(s.stages[3].mean_ns(), 30);
}

/// `latency_report_if(false)` installs no observing sink — the stats handle
/// stays at zero counts.
#[test]
fn latency_report_if_disabled_stays_empty() {
    let g = GraphBuilder::new();
    let source = g.ticker(Duration::from_millis(1)).count().map(|n: &u64| {
        Traced::with_latency(
            *n,
            TradeLatency {
                ingest: 100,
                decode: 110,
                strategy: 130,
                publish: 160,
            },
        )
    });
    let (_sink, stats) = source.latency_report_if(false, ReportOutput::Silent);
    let mut r = g.build();
    r.run(RunMode::HistoricalFrom(NanoTime::ZERO), RunFor::Cycles(3))
        .unwrap();

    let s = stats.borrow();
    for i in 1..TradeLatency::N {
        assert_eq!(s.stages[i].count, 0, "stage {i} should be unobserved");
    }
}

// ── Tier parity: stamps through `nitro!`'s compiled and nested expansions ───
//
// Closes deviation-register C7 (cutover-plan row 2.2). The stamps were
// fluent/interpreted-only because a stage is a compile-time *type*
// (`stamp::<trade_latency::ingest>()`) and `nitro!`'s dispatch forwards
// *values* — a type argument has no value to forward. `#[op(explicit = S)]`
// hoists the stage parameter to the front of the generated forwarder
// signatures, and the macro now carries the call-site turbofish onto the
// forwarder call, so the compiled and nested emissions reach the same
// `Stamp::cycle` the interpreted tier does.
//
// The assertion is deliberately on *ordering relations* rather than absolute
// nanoseconds: stamps read the wall clock, so exact values are not
// reproducible, but the invariants that make a stamp meaningful are.

wingfoil::nitro! {
    fn stamped_graph(g: &GraphBuilder) -> Stream<Traced<u64, TradeLatency>> {
        let src = g
            .ticker(PERIOD)
            .count()
            .map(|n: &u64| Traced::<u64, TradeLatency>::new(*n));
        let out = src
            .stamp::<trade_latency::ingest>()
            .stamp::<trade_latency::decode>()
            .stamp_precise::<trade_latency::strategy>();
        out
    }
}

const PERIOD: Duration = Duration::from_millis(1);
const HISTORICAL: RunMode = RunMode::HistoricalFrom(NanoTime::ZERO);

/// Every stage a chain stamps is set, and stamps advance monotonically along
/// it — the property that makes a latency record readable at all.
fn assert_stamped(value: &Traced<u64, TradeLatency>, label: &str) {
    let l = &value.latency;
    assert!(l.ingest > 0, "{label}: ingest unstamped");
    assert!(l.decode > 0, "{label}: decode unstamped");
    assert!(l.strategy > 0, "{label}: strategy unstamped");
    assert!(l.publish == 0, "{label}: publish should be untouched");
    // `stamp` reads the cycle-start snap, so two plain stamps in one cycle
    // share a timestamp; `stamp_precise` takes a fresh reading and can only
    // be at or after it. Both are `>=`, never `>`: a clock with coarse
    // resolution may return the same value twice, and asserting strict
    // inequality is how this test would go flaky on a slow CI box.
    assert_eq!(l.ingest, l.decode, "{label}: cycle-start snap is shared");
    assert!(
        l.strategy >= l.decode,
        "{label}: precise stamp went backwards ({} < {})",
        l.strategy,
        l.decode
    );
}

/// The gap that C7 named: `compiled()` over a graph containing stamps. Before
/// `explicit`/turbofish support this did not compile at all.
#[test]
fn stamps_reach_the_compiled_tier() {
    let run_for = RunFor::Cycles(6);

    let (mut runner, out) = stamped_graph::interpreted();
    runner.run(HISTORICAL, run_for).unwrap();
    let interpreted = runner.value(out);
    assert_stamped(&interpreted, "interpreted");

    let (compiled,) = stamped_graph::compiled(HISTORICAL, run_for).unwrap();
    assert_stamped(&compiled, "compiled");

    // The payload is the deterministic half — the tick count — and it must
    // agree exactly across tiers even though the stamps cannot.
    assert_eq!(
        interpreted.payload, compiled.payload,
        "payload diverged between the interpreted and compiled tiers"
    );
}

wingfoil::nitro! {
    fn stamp_island(
        g: &GraphBuilder,
        src: &Stream<Traced<u64, TradeLatency>>,
    ) -> Stream<Traced<u64, TradeLatency>> {
        let out = src
            .stamp::<trade_latency::ingest>()
            .stamp_precise::<trade_latency::strategy>();
        out
    }
}

/// The same ops inside a compiled **island** mounted in an interpreted graph —
/// the third expansion, and the one where the turbofish has to survive being
/// re-emitted inside the composite closure.
#[test]
fn stamps_reach_a_nested_island() {
    let g = GraphBuilder::new();
    let src = g
        .ticker(PERIOD)
        .count()
        .map(|n: &u64| Traced::<u64, TradeLatency>::new(*n));
    let island = stamp_island::nested(&g, &src);
    let mut runner = g.build();
    runner.run(HISTORICAL, RunFor::Cycles(6)).unwrap();

    let out = runner.value(island);
    let l = &out.latency;
    assert!(l.ingest > 0, "island: ingest unstamped");
    assert!(
        l.strategy >= l.ingest,
        "island: precise stamp went backwards"
    );
    assert_eq!(l.decode, 0, "island: decode should be untouched");
}

// ── Tier parity for the burst-shaped stamps ─────────────────────────────────
//
// `stamp_each` / `stamp_precise_each` carry `#[op(build = …, explicit = S)]`
// exactly as their scalar twins do, so they reach the compiled and nested
// expansions by the same `PhantomData<S>` mechanism. That is the claim these
// two tests exist to keep honest — a forwarder that stopped resolving would
// otherwise only be noticed by a downstream user.
//
// They live here rather than in `op_completeness.rs` for the same reason the
// scalar stamps do: that file's `nitro!` blocks are built around a plain `u64`
// surface, and a stamp needs a `Traced` payload plus a `latency_stages!`
// schema. `op_completeness.rs` records the whole latency family as pinned
// here, the way it records `poll` as pinned by `poll_all_tiers.rs`.

wingfoil::nitro! {
    fn stamped_burst_graph(g: &GraphBuilder) -> Stream<Burst<Traced<u64, TradeLatency>>> {
        let src = g
            .ticker(PERIOD)
            .count()
            .map(|n: &u64| -> Burst<Traced<u64, TradeLatency>> {
                burst![
                    Traced::<u64, TradeLatency>::new(*n),
                    Traced::<u64, TradeLatency>::new(*n + 1000),
                ]
            });
        let out = src
            .stamp_each::<trade_latency::ingest>()
            .stamp_precise_each::<trade_latency::strategy>();
        out
    }
}

/// `compiled()` over a graph whose stamps are burst-shaped. Every value in the
/// burst must come out stamped — the whole point of the op — and the compiled
/// tier must agree with the interpreted one.
#[test]
fn burst_stamps_reach_the_compiled_tier() {
    let run_for = RunFor::Cycles(6);

    let (compiled,) = stamped_burst_graph::compiled(HISTORICAL, run_for).unwrap();
    assert_eq!(2, compiled.len(), "compiled: burst lost values");
    for v in compiled.iter() {
        assert!(v.latency.ingest > 0, "compiled: ingest unstamped");
        assert!(
            v.latency.strategy >= v.latency.ingest,
            "compiled: precise stamp went backwards"
        );
        assert_eq!(0, v.latency.decode, "compiled: decode should be untouched");
    }

    let (mut runner, out) = stamped_burst_graph::interpreted();
    runner.run(HISTORICAL, run_for).unwrap();
    let interpreted = runner.value(out);
    assert_eq!(
        interpreted.iter().map(|v| v.payload).collect::<Vec<_>>(),
        compiled.iter().map(|v| v.payload).collect::<Vec<_>>(),
        "payload diverged between the interpreted and compiled tiers"
    );
}

wingfoil::nitro! {
    fn stamp_each_island(
        g: &GraphBuilder,
        src: &Stream<Burst<Traced<u64, TradeLatency>>>,
    ) -> Stream<Burst<Traced<u64, TradeLatency>>> {
        let out = src
            .stamp_each::<trade_latency::ingest>()
            .stamp_precise_each::<trade_latency::strategy>();
        out
    }
}

/// The same ops inside a compiled **island** — the third expansion, and the one
/// where the stage turbofish has to survive re-emission inside the composite.
#[test]
fn burst_stamps_reach_a_nested_island() {
    let g = GraphBuilder::new();
    let src = g
        .ticker(PERIOD)
        .count()
        .map(|n: &u64| -> Burst<Traced<u64, TradeLatency>> {
            burst![
                Traced::<u64, TradeLatency>::new(*n),
                Traced::<u64, TradeLatency>::new(*n + 1000),
            ]
        });
    let island = stamp_each_island::nested(&g, &src);
    let mut runner = g.build();
    runner.run(HISTORICAL, RunFor::Cycles(6)).unwrap();

    let out = runner.value(island);
    assert_eq!(2, out.len(), "island: burst lost values");
    for v in out.iter() {
        assert!(v.latency.ingest > 0, "island: ingest unstamped");
        assert!(
            v.latency.strategy >= v.latency.ingest,
            "island: precise stamp went backwards"
        );
        assert_eq!(0, v.latency.decode, "island: decode should be untouched");
    }
}

// ── Stamping: one mode argument instead of a method per combination ─────────

/// `stamp_as` is the general form the four named stamps are shorthands for,
/// and it agrees with each of them exactly — `wall_time` is snapped once per
/// cycle, so two nodes stamping in the same cycle write the identical value.
#[test]
fn stamp_as_agrees_with_the_named_stamps() {
    let g = GraphBuilder::new();
    let src = traced_source(&g);
    let named = src.stamp::<trade_latency::ingest>().accumulate();
    let by_mode = src
        .stamp_as::<trade_latency::ingest>(Stamping::Cycle)
        .accumulate();
    let mut r = g.build();
    r.run(RunMode::HistoricalFrom(NanoTime::ZERO), RunFor::Cycles(3))
        .unwrap();

    let (named, by_mode) = (r.value(&named), r.value(&by_mode));
    assert_eq!(3, named.len());
    for (a, b) in named.iter().zip(by_mode.iter()) {
        assert!(a.latency.ingest > 0);
        assert_eq!(a.latency.ingest, b.latency.ingest);
    }
}

/// `Stamping::Off` wires no node — the stage stays unset. The same behaviour
/// `stamp_disabled_is_identity` pins through `Stamping::on_if(false)`, reached
/// here by naming the variant directly.
#[test]
fn stamping_off_writes_nothing() {
    let g = GraphBuilder::new();
    let acc = traced_source(&g)
        .stamp_as::<trade_latency::ingest>(Stamping::Off)
        .accumulate();
    let mut r = g.build();
    r.run(RunMode::HistoricalFrom(NanoTime::ZERO), RunFor::Cycles(1))
        .unwrap();
    assert_eq!(0, r.value(&acc)[0].latency.ingest);
}

/// The two-bool config case `Stamping::new` exists for, and the polarity
/// hazard it removed: one mode value cannot stamp a stage twice, where the
/// pair of `_if` calls this replaced would if a `!` were dropped.
#[test]
fn stamping_resolves_the_two_config_flags() {
    assert_eq!(Stamping::Off, Stamping::new(false, false));
    assert_eq!(Stamping::Off, Stamping::new(false, true));
    assert_eq!(Stamping::Cycle, Stamping::new(true, false));
    assert_eq!(Stamping::Precise, Stamping::new(true, true));
    assert_eq!(Stamping::Precise, Stamping::precise_if(true));
    assert_eq!(Stamping::Cycle, Stamping::precise_if(false));
    assert_eq!(Stamping::Cycle, Stamping::on_if(true));
    assert_eq!(Stamping::Off, Stamping::on_if(false));
    assert!(!Stamping::Off.is_on());
    assert!(Stamping::Cycle.is_on() && Stamping::Precise.is_on());
    assert_eq!(Stamping::Cycle, Stamping::default());
}

// ── stamp_all: N stages, one node, identical numbers ───────────────────────

/// The whole claim behind fusing: `stamp_all` is not an approximation of the
/// chained form, it is the same numbers from one node. Under `Cycle` both
/// share the per-cycle snap, so the two chains agree stamp for stamp.
#[test]
fn stamp_all_matches_the_chained_form_exactly() {
    let g = GraphBuilder::new();
    let src = traced_source(&g);
    let fused = src
        .stamp_all::<(trade_latency::ingest, trade_latency::decode)>(Stamping::Cycle)
        .accumulate();
    let chained = src
        .stamp::<trade_latency::ingest>()
        .stamp::<trade_latency::decode>()
        .accumulate();
    let mut r = g.build();
    r.run(RunMode::HistoricalFrom(NanoTime::ZERO), RunFor::Cycles(3))
        .unwrap();

    let (fused, chained) = (r.value(&fused), r.value(&chained));
    assert_eq!(3, fused.len());
    for (f, c) in fused.iter().zip(chained.iter()) {
        assert!(f.latency.ingest > 0);
        assert_eq!(f.latency.ingest, c.latency.ingest);
        assert_eq!(f.latency.decode, c.latency.decode);
        // Same cycle, cached clock: the two stages share the snap.
        assert_eq!(f.latency.ingest, f.latency.decode);
        // Untouched stages stay unset.
        assert_eq!(0, f.latency.strategy);
    }
}

/// Fusing does **not** collapse a precise stamp: each stage in the set still
/// takes its own clock read, which is what makes `stamp_all` a free
/// substitution rather than a trade-off.
#[test]
fn stamp_all_takes_one_clock_read_per_stage_when_precise() {
    let g = GraphBuilder::new();
    let acc = traced_source(&g)
        .stamp_all::<(
            trade_latency::ingest,
            trade_latency::decode,
            trade_latency::strategy,
        )>(Stamping::Precise)
        .accumulate();
    let mut r = g.build();
    r.run(RunMode::HistoricalFrom(NanoTime::ZERO), RunFor::Cycles(2))
        .unwrap();

    for v in r.value(&acc) {
        let l = v.latency;
        assert!(l.ingest > 0, "every stage in the set is stamped");
        assert!(l.decode >= l.ingest, "and in tuple order");
        assert!(l.strategy >= l.decode);
        assert!(
            l.strategy > l.ingest,
            "distinct reads, not one shared snap: {l:?}"
        );
    }
}

/// `Stamping::Off` short-circuits the fused form too.
#[test]
fn stamp_all_off_wires_no_node() {
    let g = GraphBuilder::new();
    let acc = traced_source(&g)
        .stamp_all::<(trade_latency::ingest, trade_latency::decode)>(Stamping::Off)
        .accumulate();
    let mut r = g.build();
    r.run(RunMode::HistoricalFrom(NanoTime::ZERO), RunFor::Cycles(1))
        .unwrap();
    let l = r.value(&acc)[0].latency;
    assert_eq!((0, 0), (l.ingest, l.decode));
}

// ── The handle: read out, reset, window ────────────────────────────────────

/// A fully stamped run, read back through the handle rather than by indexing
/// `stages` — labelled hops, an end-to-end total, and no `1..N` convention for
/// the caller to reproduce.
#[test]
fn the_handle_reads_out_labelled_hops_and_an_end_to_end_total() {
    let g = GraphBuilder::new();
    let source = g.ticker(Duration::from_millis(1)).count().map(|n: &u64| {
        Traced::with_latency(
            *n,
            TradeLatency {
                ingest: 100,
                decode: 110,
                strategy: 130,
                publish: 160,
            },
        )
    });
    let (_sink, latency) = source.latency_report(ReportOutput::Silent);
    let mut r = g.build();
    r.run(RunMode::HistoricalFrom(NanoTime::ZERO), RunFor::Cycles(3))
        .unwrap();

    let hops = latency.hops();
    assert_eq!(3, hops.len(), "three hops across four stages");
    assert_eq!(("ingest", "decode"), (hops[0].from, hops[0].to));
    assert_eq!((3, 10), (hops[0].count, hops[0].mean_ns));
    assert_eq!((3, 20), (hops[1].count, hops[1].mean_ns));
    assert_eq!((3, 30), (hops[2].count, hops[2].mean_ns));

    let total = latency.total();
    assert_eq!(("ingest", "publish"), (total.from, total.to));
    assert_eq!((3, 60), (total.count, total.mean_ns));

    let snapshot = latency.snapshot();
    assert_eq!(hops, snapshot.hops);
    assert_eq!(Some(&hops[1]), snapshot.hop("strategy"));
    assert_eq!(None, snapshot.hop("nonesuch"));

    let report = latency.report();
    assert!(
        report.contains("ingest -> publish (end to end)"),
        "{report}"
    );
    assert!(report.contains("p99.9"), "{report}");
}

/// `take` is snapshot-then-reset: the numbers come out once, and what is left
/// behind is empty. Without it a p99 gauge records the worst moment the
/// process ever had rather than the state it is in.
#[test]
fn take_reads_the_numbers_out_and_leaves_the_accumulator_empty() {
    let g = GraphBuilder::new();
    let source = g.ticker(Duration::from_millis(1)).count().map(|n: &u64| {
        Traced::with_latency(
            *n,
            TradeLatency {
                ingest: 100,
                decode: 150,
                strategy: 150,
                publish: 0,
            },
        )
    });
    let (_sink, latency) = source.latency_report(ReportOutput::Silent);
    let mut r = g.build();
    r.run(RunMode::HistoricalFrom(NanoTime::ZERO), RunFor::Cycles(4))
        .unwrap();

    let first = latency.take();
    assert_eq!(4, first.hops[0].count, "ingest -> decode measured");
    assert_eq!(
        4, first.hops[1].same_instant,
        "decode -> strategy same-cycle"
    );
    assert_eq!(
        4, first.hops[2].unstamped,
        "strategy -> publish never stamped"
    );
    assert_eq!(4, first.total.unstamped, "and so is the end-to-end total");

    let second = latency.take();
    for hop in &second.hops {
        assert_eq!(0, hop.observations(), "reset left {} empty", hop.label());
    }
    assert_eq!(0, second.total.observations());
}

/// `windows` is the shape a gauge wants: each snapshot covers only its own
/// period. A cumulative stream over the same run would read 2, 4, 6.
#[test]
fn windows_emits_per_window_snapshots_not_cumulative_ones() {
    let g = GraphBuilder::new();
    let source = g.ticker(Duration::from_millis(1)).count().map(|n: &u64| {
        Traced::with_latency(
            *n,
            TradeLatency {
                ingest: 100,
                decode: 110,
                strategy: 130,
                publish: 160,
            },
        )
    });
    let (_sink, latency) = source.latency_report(ReportOutput::Silent);
    let windows = latency.windows(&g, Duration::from_millis(2)).accumulate();
    let mut r = g.build();
    r.run(RunMode::HistoricalFrom(NanoTime::ZERO), RunFor::Cycles(6))
        .unwrap();

    let counts: Vec<u64> = r.value(&windows).iter().map(|s| s.total.count).collect();
    assert_eq!(3, counts.len(), "one snapshot per 2 ms window over 6 ms");
    assert!(
        counts.iter().all(|c| *c <= 2),
        "each window covers only its own period, got {counts:?}"
    );
    assert!(
        counts.iter().sum::<u64>() >= 4,
        "and between them they account for the run, got {counts:?}"
    );
}

/// The windowed read is destructive, so a second `windows` stream over the
/// same accumulator would silently steal samples from the first — wiring one
/// fails loudly instead.
#[test]
#[should_panic(expected = "windows() already wired")]
fn a_second_windows_stream_on_one_handle_panics_at_wiring() {
    let g = GraphBuilder::new();
    let source = traced_source(&g);
    let (_sink, latency) = source.latency_report(ReportOutput::Silent);
    let _gauges = latency.windows(&g, Duration::from_millis(1));
    let _log = latency.windows(&g, Duration::from_secs(60));
}

/// The diagnosis this whole tally exists for, end to end through the engine:
/// two `stamp`s in one cycle cannot measure the hop between them, and the
/// report says so instead of printing a row of zeros.
#[test]
fn same_cycle_stamps_are_reported_as_unmeasured_not_as_zero() {
    let g = GraphBuilder::new();
    let stamped = traced_source(&g)
        .stamp::<trade_latency::ingest>()
        .stamp::<trade_latency::decode>();
    let (_sink, latency) = stamped.latency_report(ReportOutput::Silent);
    let mut r = g.build();
    r.run(RunMode::HistoricalFrom(NanoTime::ZERO), RunFor::Cycles(5))
        .unwrap();

    let hop = latency.hops()[0];
    assert_eq!(0, hop.count, "nothing was measured");
    assert_eq!(
        5, hop.same_instant,
        "but every observation is accounted for"
    );
    assert_eq!(0, hop.mean_ns);

    let report = latency.report();
    let row = report
        .lines()
        .find(|l| l.contains("ingest -> decode"))
        .expect("the row is printed");
    assert!(row.contains("5 same-cycle"), "{row}");

    // The same wiring with precise stamps *does* measure it.
    let g = GraphBuilder::new();
    let precise = traced_source(&g)
        .stamp_all::<(trade_latency::ingest, trade_latency::decode)>(Stamping::Precise);
    let (_sink, latency) = precise.latency_report(ReportOutput::Silent);
    let mut r = g.build();
    r.run(RunMode::HistoricalFrom(NanoTime::ZERO), RunFor::Cycles(5))
        .unwrap();
    let hop = latency.hops()[0];
    // Precise stamps read the clock once per stage, so the hop is measurable —
    // but on a monotonic clock that ticks coarser than the ~20ns between the
    // two reads (24-50MHz ARM counters), a pair can still return equal
    // nanoseconds and land in `same_instant`. What is portable is that every
    // observation is accounted for and none is rejected outright.
    assert_eq!(
        5,
        hop.count + hop.same_instant,
        "every observation is a measurement or a same-instant collision"
    );
    assert_eq!(0, hop.backwards);
    assert_eq!(0, hop.unstamped);
}
