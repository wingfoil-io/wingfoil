//! The busy-spin ingestion pattern: a `poll` source is cycled on **every**
//! engine cycle (`Caps::ALWAYS`), and a realtime run containing one never
//! parks — the kernel spins cycles back-to-back, polling each time. This is
//! the latency-critical pattern (ring buffers, sockets, `try_recv`), and it
//! is lossless and ordered — one value per cycle, no coalescing — in
//! contrast to `external`, which collapses queued values to latest-wins.

use std::sync::mpsc;
use std::time::Duration;

use wingfoil::{RunFor, RunMode};
use wingfoil_next::fluent::GraphBuilder;
use wingfoil_next::op::{Caps, Op};
use wingfoil_next::ops::Poll;

/// A producer thread feeds a channel; the graph busy-polls it with
/// `try_recv`. Every value arrives, in order — the lossless contrast to
/// `external`'s latest-wins coalescing.
#[test]
fn poll_source_is_lossless_and_ordered() {
    let (tx, rx) = mpsc::channel::<u64>();
    let g = GraphBuilder::new();
    let values = g.poll(move || rx.try_recv().ok());
    let acc = values.accumulate();

    let producer = std::thread::spawn(move || {
        for i in 1..=100 {
            tx.send(i).expect("runner alive for the send window");
        }
    });
    let mut r = g.build();
    r.run(
        RunMode::RealTime,
        RunFor::Duration(Duration::from_millis(50)),
    )
    .unwrap();
    producer.join().expect("producer thread");

    // The spin loop polls far faster than the producer sends: every value
    // must arrive, in order — no coalescing.
    assert_eq!((1..=100).collect::<Vec<u64>>(), r.value(&acc));
}

/// A spin run still honours the cycle bound: with nothing arriving, it
/// spins exactly N cycles and returns (rather than parking forever).
#[test]
fn spin_run_honours_cycle_bound() {
    let g = GraphBuilder::new();
    let values = g.poll(|| None::<u64>);
    let count = values.map(|v| *v).accumulate();
    let mut r = g.build();
    r.run(RunMode::RealTime, RunFor::Cycles(10_000)).unwrap();
    assert!(r.value(&count).is_empty());
}

/// Poll sources coexist with scheduled work under spin: the ticker's
/// callbacks still fire at (at least) their scheduled times while the
/// kernel spins.
#[test]
fn spin_run_still_fires_scheduled_callbacks() {
    let (tx, rx) = mpsc::channel::<u64>();
    drop(tx); // never sends — the poll source stays quiet
    let g = GraphBuilder::new();
    let _quiet = g.poll(move || rx.try_recv().ok());
    let ticks = g.ticker(Duration::from_millis(5)).count();
    let mut r = g.build();
    r.run(
        RunMode::RealTime,
        RunFor::Duration(Duration::from_millis(24)),
    )
    .unwrap();
    let n = r.value(&ticks);
    assert!(n >= 4, "expected >= 4 ticker fires in 24ms at 5ms, got {n}");
}

/// The capability is declared statically, like every other cap.
#[test]
fn poll_caps_are_always() {
    const {
        assert!(<Poll<u64, fn() -> Option<u64>> as Op>::CAPS.always);
        assert!(!<Poll<u64, fn() -> Option<u64>> as Op>::CAPS.callback_activated());
        assert!(!Caps::SCHEDULES.always);
    }
}

/// Historical replay has nothing to busy-poll; the combination is rejected.
#[test]
#[should_panic(expected = "require RunMode::RealTime")]
fn poll_rejects_historical_mode() {
    let g = GraphBuilder::new();
    let _values = g.poll(|| None::<u64>);
    let mut r = g.build();
    r.run(
        RunMode::HistoricalFrom(wingfoil::NanoTime::ZERO),
        RunFor::Cycles(1),
    )
    .unwrap();
}
