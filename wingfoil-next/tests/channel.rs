//! Phase 3 channel layer: cross-thread value transport with the [`Message`]
//! envelope, ported onto the Op model. Sources emit **bursts** (every value,
//! grouped by instant — never latest-wins), run in **both** modes (realtime
//! waker-driven, historical deterministic replay on the graph clock), and a
//! producer error propagates into the graph via the Phase 0.1 fallible cycle.

use std::time::Duration;

use wingfoil::{NanoTime, RunFor, RunMode};
use wingfoil_next::channel::Message;
use wingfoil_next::fluent::GraphBuilder;

/// A producer thread sends values through the channel; the graph receives
/// them as bursts, losslessly and in order — nothing coalesced.
#[test]
fn channel_delivers_all_values_across_threads() {
    let g = GraphBuilder::new();
    let (values, sender) = g.channel::<u64>();
    let acc = values.collapse_accumulate();
    let mut r = g.build();

    let producer = std::thread::spawn(move || {
        for i in 1..=5 {
            sender.send(i);
            std::thread::sleep(Duration::from_millis(2));
        }
    });
    // Generous bound: with a burst source no value is dropped, so all five
    // arrive across however many cycles the scheduler grants.
    r.run(RunMode::RealTime, RunFor::Cycles(50)).unwrap();
    producer.join().expect("producer thread");

    let got = r.value(&acc);
    assert_eq!((1..=5).collect::<Vec<u64>>(), got, "all values, in order");
}

/// A channel drives a **historical** replay: the producer sends timestamped
/// values (with wall-clock delays) then closes, and the receiver replays them
/// deterministically at their graph timestamps as bursts — the classic
/// `produce_async` model. This is the case my first cut wrongly rejected.
#[test]
fn channel_replays_deterministically_in_historical_mode() {
    let g = GraphBuilder::new();
    let (values, sender) = g.channel::<u64>();
    let acc = values.with_time().accumulate();
    let mut r = g.build();

    let producer = std::thread::spawn(move || {
        sender.send_at(10, NanoTime::new(100));
        std::thread::sleep(Duration::from_millis(5));
        sender.send_at(20, NanoTime::new(200));
        sender.send_at(30, NanoTime::new(300));
        sender.close();
    });

    r.run(RunMode::HistoricalFrom(NanoTime::ZERO), RunFor::Forever)
        .unwrap();
    producer.join().expect("producer thread");

    // Each distinct timestamp is one single-value burst at exactly its time.
    let got = r.value(&acc);
    let flat: Vec<(NanoTime, Vec<u64>)> = got
        .into_iter()
        .map(|(t, b)| (t, b.iter().copied().collect()))
        .collect();
    assert_eq!(
        vec![
            (NanoTime::new(100), vec![10]),
            (NanoTime::new(200), vec![20]),
            (NanoTime::new(300), vec![30]),
        ],
        flat
    );
}

/// Same-time historical values ride **one atomic burst** at that timestamp —
/// never coalesced, never split across the clock (the burst pattern, not the
/// monotonic-bump fallback).
#[test]
fn channel_historical_same_time_values_ride_one_burst() {
    let g = GraphBuilder::new();
    let (values, sender) = g.channel::<u64>();
    let acc = values.with_time().accumulate();
    let mut r = g.build();

    let producer = std::thread::spawn(move || {
        sender.send_at(1, NanoTime::new(100));
        sender.send_at(2, NanoTime::new(100));
        sender.send_at(3, NanoTime::new(200));
        sender.close();
    });
    r.run(RunMode::HistoricalFrom(NanoTime::ZERO), RunFor::Forever)
        .unwrap();
    producer.join().expect("producer thread");

    let got: Vec<(NanoTime, Vec<u64>)> = r
        .value(&acc)
        .into_iter()
        .map(|(t, b)| (t, b.iter().copied().collect()))
        .collect();
    // t=100 carries both 1 and 2 in one burst; t=200 carries 3.
    assert_eq!(
        vec![
            (NanoTime::new(100), vec![1, 2]),
            (NanoTime::new(200), vec![3]),
        ],
        got
    );
}

/// A producer that sends `Message::Error` aborts the receiving graph's run,
/// surfaced through the channel node's context — the channel layer leaning on
/// Phase 0.1's fallible cycle.
#[test]
fn channel_error_aborts_the_run() {
    let g = GraphBuilder::new();
    let (values, sender) = g.channel::<u64>();
    let _acc = values.collapse_accumulate();
    let mut r = g.build();

    let producer = std::thread::spawn(move || {
        sender.send(1);
        std::thread::sleep(Duration::from_millis(2));
        sender.send_error(anyhow::anyhow!("producer blew up"));
    });
    let result = r.run(RunMode::RealTime, RunFor::Cycles(50));
    producer.join().expect("producer thread");

    let err = result.expect_err("a producer error must abort the run");
    let msg = format!("{err:#}");
    assert!(
        msg.contains("channel"),
        "error names the channel node: {msg}"
    );
    assert!(
        msg.contains("producer blew up"),
        "error chains cause: {msg}"
    );
}

/// The message envelope's equality mirrors the classic `Message`: values and
/// checkpoints compare structurally, end-of-stream is a unit, errors never
/// compare equal.
#[test]
fn message_equality_matches_classic() {
    assert_eq!(Message::<u64>::Value(7), Message::Value(7));
    assert_ne!(Message::<u64>::Value(7), Message::Value(8));
    let t = NanoTime::new(42);
    assert_eq!(Message::<u64>::Checkpoint(t), Message::Checkpoint(t));
    assert_eq!(Message::<u64>::EndOfStream, Message::EndOfStream);
    let e1 = Message::<u64>::Error(std::sync::Arc::new(anyhow::anyhow!("x")));
    let e2 = Message::<u64>::Error(std::sync::Arc::new(anyhow::anyhow!("x")));
    assert_ne!(e1, e2);
}
