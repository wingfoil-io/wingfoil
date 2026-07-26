//! Latency capture parity tests — ports the node-layer cases from classic
//! `wingfoil::latency` to the wingfoil-next Op engine. The pure data layer
//! (`Traced`/`Latency`/`Stage`/`StageStats`/`LatencyStats` layout + arithmetic)
//! is reused verbatim from the classic crate and is covered by its own unit
//! tests; here we assert the *engine* behaviour: that `stamp`/`stamp_precise`
//! write wall-clock time (shared per cycle, fresh for `_precise`) and that
//! `latency_report` aggregates per-stage deltas.
//!
//! Sources are synthesised with `ticker().count().map(..)` — the delivery time
//! is irrelevant here (latency records ride inside the payload and stamps read
//! the wall clock), so this avoids the burst-grouped `channel` source and its
//! producer threads while exercising the identical stamping / aggregation path.

use std::time::Duration;

use wingfoil::{NanoTime, RunFor, RunMode};
use wingfoil_next::latency::*;
use wingfoil_next::prelude::*;

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

// ── Re-export / macro-in-next sanity ────────────────────────────────────────
// The derive is engine-agnostic and re-exported unchanged; these confirm the
// macro expands and the traits resolve inside the next crate.

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

/// Classic `stamp_stream_writes_wall_time_into_named_stage`: stamps use
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

/// Classic `stamp_works_identically_in_historical_and_realtime`: same wiring,
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

/// Classic `stamp_if_disabled_inserts_no_node`: `stamp_if(false)` is identity —
/// the stage it would have written stays zero.
#[test]
fn stamp_if_disabled_is_identity() {
    let g = GraphBuilder::new();
    let acc = traced_source(&g)
        .stamp_if::<trade_latency::ingest>(false)
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

/// Classic `stamp_precise_writes_fresh_timestamps`: two `stamp_precise`
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

/// Classic `multiple_stamps_compose`: three cached-`stamp` wrappers in the same
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

/// Classic `latency_report_aggregates_across_ticks`: three fully-stamped
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
    let (_sink, stats) = source.latency_report(false);
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
    let (_sink, stats) = source.latency_report_if(false, false);
    let mut r = g.build();
    r.run(RunMode::HistoricalFrom(NanoTime::ZERO), RunFor::Cycles(3))
        .unwrap();

    let s = stats.borrow();
    for i in 1..TradeLatency::N {
        assert_eq!(s.stages[i].count, 0, "stage {i} should be unobserved");
    }
}
