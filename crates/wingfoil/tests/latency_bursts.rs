//! Burst-shaped latency ops: `stamp_each`, `stamp_precise_each`, and the
//! `Stream<Burst<P>>` impl of `latency_report`.
//!
//! These exist so a stamped pipeline fed by an adapter can stay burst-shaped
//! instead of `collapse()`-ing first. `collapse` keeps only the **last** value
//! of each burst, so on an ingest path carrying orders, fills or control
//! messages it is silent data loss — and it bites precisely under load, when a
//! producer outruns the graph cycle and bursts stop being single-item.
//!
//! Every test below therefore asserts the burst path against the *scalar +
//! collapse* path it replaces, so the thing being prevented is visible in the
//! assertions rather than only in a comment.

use std::time::Duration;

use wingfoil::latency::{
    Latency, LatencyBurstStreamOps, LatencyReportOps, LatencyStreamOps, Stage, Traced,
    latency_stages,
};
use wingfoil::prelude::*;
use wingfoil::{NanoTime, RunFor, RunMode};

const HISTORICAL: RunMode = RunMode::HistoricalFrom(NanoTime::ZERO);

latency_stages! {
    pub HopLatency { ingress, egress }
}

type Msg = Traced<u64, HopLatency>;

/// Three payloads per burst, so "all but the last" is unambiguous.
fn bursts(g: &GraphBuilder) -> Stream<Burst<Msg>> {
    g.ticker(Duration::from_nanos(10)).count().map(|c: &u64| {
        let base = *c * 10;
        burst![Msg::new(base), Msg::new(base + 1), Msg::new(base + 2)]
    })
}

#[test]
fn stamp_each_stamps_every_value_and_keeps_them_all() {
    let g = GraphBuilder::new();
    let src = bursts(&g);

    let stamped = src.stamp_each::<hop_latency::ingress>().accumulate();
    // What the pipeline used to do: collapse, then stamp one value.
    let collapsed = src
        .collapse::<Msg>()
        .stamp::<hop_latency::ingress>()
        .accumulate();

    let mut r = g.build();
    r.run(HISTORICAL, RunFor::Cycles(3)).unwrap();

    let burst_values = r.value(&stamped);
    assert_eq!(3, burst_values.len(), "one burst per cycle");
    for (cycle, group) in burst_values.iter().enumerate() {
        let base = (cycle as u64 + 1) * 10;
        assert_eq!(
            vec![base, base + 1, base + 2],
            group.iter().map(|m| m.payload).collect::<Vec<_>>(),
            "every payload in the burst survives",
        );
        for m in group.iter() {
            assert!(m.latency.ingress > 0, "every value carries a stamp");
        }
    }

    // The path being replaced keeps exactly one of the three per cycle — the
    // last — which is the data loss these ops exist to remove.
    let kept = r.value(&collapsed);
    assert_eq!(
        vec![12, 22, 32],
        kept.iter().map(|m| m.payload).collect::<Vec<_>>(),
        "collapse keeps only the burst's last value",
    );
}

#[test]
fn stamp_each_reads_the_clock_once_per_burst() {
    let g = GraphBuilder::new();
    let stamped = bursts(&g).stamp_each::<hop_latency::ingress>().accumulate();

    let mut r = g.build();
    r.run(HISTORICAL, RunFor::Cycles(2)).unwrap();

    // A burst is one instant's worth of values, so a per-value clock read would
    // invent differences that do not exist. All three share one stamp.
    for group in r.value(&stamped) {
        let stamps: Vec<u64> = group.iter().map(|m| m.latency.ingress).collect();
        assert!(
            stamps.windows(2).all(|w| w[0] == w[1]),
            "values in one burst share a stamp, got {stamps:?}",
        );
    }
}

#[test]
fn stamp_precise_each_separates_stages_not_values() {
    let g = GraphBuilder::new();
    let stamped = bursts(&g)
        .stamp_precise_each::<hop_latency::ingress>()
        .stamp_precise_each::<hop_latency::egress>()
        .accumulate();

    let mut r = g.build();
    r.run(HISTORICAL, RunFor::Cycles(2)).unwrap();

    for group in r.value(&stamped) {
        for m in group.iter() {
            // Distinct *stages* in one cycle get distinct timestamps — that is
            // what precise stamping is for.
            assert!(
                m.latency.egress > m.latency.ingress,
                "precise stamps advance between stages: {:?}",
                m.latency,
            );
        }
        // ...but within a stage, one burst is still one instant.
        let ingress: Vec<u64> = group.iter().map(|m| m.latency.ingress).collect();
        assert!(ingress.windows(2).all(|w| w[0] == w[1]));
    }
}

#[test]
fn stamp_each_if_false_inserts_no_node() {
    let g = GraphBuilder::new();
    let src = bursts(&g);
    let passthrough = src
        .stamp_each_if::<hop_latency::ingress>(false)
        .accumulate();

    let mut r = g.build();
    r.run(HISTORICAL, RunFor::Cycles(2)).unwrap();

    for group in r.value(&passthrough) {
        for m in group.iter() {
            assert_eq!(0, m.latency.ingress, "disabled stamp writes nothing");
        }
    }
}

#[test]
fn latency_report_over_bursts_observes_every_value() {
    let g = GraphBuilder::new();
    let src = bursts(&g)
        .stamp_precise_each::<hop_latency::ingress>()
        .stamp_precise_each::<hop_latency::egress>();

    let (_burst_sink, burst_stats) = src.latency_report(false);
    // The path being replaced: collapse first, and two of every three samples
    // never reach the histogram.
    let (_scalar_sink, scalar_stats) = src.collapse::<Msg>().latency_report(false);

    let mut r = g.build();
    r.run(HISTORICAL, RunFor::Cycles(4)).unwrap();

    // Stage 1 is the ingress -> egress hop; stage 0 has no predecessor.
    let observed = burst_stats.borrow().stages[1].count;
    let collapsed = scalar_stats.borrow().stages[1].count;
    assert_eq!(12, observed, "4 cycles x 3 values, every one sampled");
    assert_eq!(4, collapsed, "collapse loses two samples of every three");
}
