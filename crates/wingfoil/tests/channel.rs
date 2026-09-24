//! Phase 3 channel layer: cross-thread value transport with the [`Message`]
//! envelope, ported onto the Op model. Sources emit **bursts** (every value,
//! grouped by instant — never latest-wins), run in **both** modes (realtime
//! waker-driven, historical deterministic replay on the graph clock), and a
//! producer error propagates into the graph via the Phase 0.1 fallible cycle.

use std::time::Duration;

use wingfoil::channel::Message;
use wingfoil::prelude::*;
use wingfoil::{NanoTime, RunFor, RunMode};

/// `with_time().accumulate()` on a burst source, flattened so the whole
/// sequence compares as `(time, values)` pairs (a `Burst` is a `TinyVec`, so
/// it is not directly comparable to a `Vec`).
fn timed(v: Vec<(NanoTime, Burst<u64>)>) -> Vec<(NanoTime, Vec<u64>)> {
    v.into_iter()
        .map(|(t, b)| (t, b.iter().copied().collect()))
        .collect()
}

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
/// deterministically at their graph timestamps as bursts — the legacy
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

/// The **realtime** counterpart of the test above, and deliberately the
/// opposite result: a realtime burst groups by **arrival**, not by timestamp.
/// Two values stamped at the same instant land in two separate bursts if the
/// graph cycles between the sends — the timestamp is ignored entirely in this
/// mode (`Message::ValueAt` is treated as `Message::Value`).
///
/// Pinned because the asymmetry is easy to forget and expensive to rediscover:
/// an `etcd_sub` snapshot stamps every key with one instant, and the etcd
/// integration suite flaked intermittently on the assumption that this made it
/// one burst. It does not — nothing is lost, but the cycle a value lands on is
/// wall-clock luck, so a realtime consumer that must see every value bounds its
/// run by `RunFor::Duration`/`Forever` and accumulates, never by
/// `RunFor::Cycles(n)`.
///
/// Deterministic despite being a race in the wild: the producer waits for the
/// graph to *tell it* the first burst was delivered before sending the second.
#[test]
fn channel_realtime_groups_by_arrival_not_by_timestamp() {
    let g = GraphBuilder::new();
    let (values, sender) = g.channel::<u64>();
    // The graph signals the producer from inside the cycle that delivers a
    // burst, which is what makes the split below happen every time rather than
    // only under load.
    let (delivered_tx, delivered_rx) = std::sync::mpsc::channel::<()>();
    let bursts = values
        .map(move |b: &Burst<u64>| {
            let _ = delivered_tx.send(());
            b.iter().copied().collect::<Vec<u64>>()
        })
        .accumulate();
    let mut r = g.build();

    let producer = std::thread::spawn(move || {
        sender.send_at(1, NanoTime::new(100));
        delivered_rx.recv().expect("first burst delivered");
        // Same timestamp as the first — and still a burst of its own.
        sender.send_at(2, NanoTime::new(100));
        sender.close();
    });
    // Terminates on the producer's `close()` (and its dropped sender), so the
    // bound only has to be "long enough": `Forever` is exact here.
    r.run(RunMode::RealTime, RunFor::Forever).unwrap();
    producer.join().expect("producer thread");

    assert_eq!(
        vec![vec![1], vec![2]],
        r.value(&bursts),
        "same-time realtime values split by arrival (contrast the historical test above)"
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

/// The message envelope's equality mirrors the legacy `Message`: values and
/// checkpoints compare structurally, end-of-stream is a unit, errors never
/// compare equal.
#[test]
fn message_equality_matches_legacy() {
    assert_eq!(Message::<u64>::Value(7), Message::Value(7));
    assert_ne!(Message::<u64>::Value(7), Message::Value(8));
    let t = NanoTime::new(42);
    assert_eq!(Message::<u64>::Checkpoint(t), Message::Checkpoint(t));
    assert_eq!(Message::<u64>::EndOfStream, Message::EndOfStream);
    let e1 = Message::<u64>::Error(std::sync::Arc::new(anyhow::anyhow!("x")));
    let e2 = Message::<u64>::Error(std::sync::Arc::new(anyhow::anyhow!("x")));
    assert_ne!(e1, e2);
}

/// `channel_bounded(Some(n))`: a concurrent producer sends more values than the
/// bound, so its `send` blocks (back-pressure) until the graph drains — yet the
/// historical replay is lossless and deterministic, identical to the unbounded
/// channel. (The producer runs on its own thread, so it is drained *during* the
/// run; a producer that queued everything before the run must stay unbounded.)
#[test]
fn channel_bounded_applies_backpressure_without_changing_the_result() {
    let g = GraphBuilder::new();
    let (values, sender) = g.channel_bounded::<u64>(Some(2));
    let acc = values.with_time().accumulate();
    let mut r = g.build();

    let producer = std::thread::spawn(move || {
        // 8 values, buffer of 2 → the producer blocks after the 2nd until the
        // graph reads, and so on. All must still arrive at their timestamps.
        for i in 1..=8u64 {
            sender.send_at(i * 10, NanoTime::new(i * 100));
        }
        sender.close();
    });

    r.run(RunMode::HistoricalFrom(NanoTime::ZERO), RunFor::Forever)
        .unwrap();
    producer.join().expect("producer thread");

    let got = r.value(&acc);
    let values: Vec<u64> = got.iter().flat_map(|(_, v)| v.clone()).collect();
    let times: Vec<u64> = got.iter().map(|(t, _)| u64::from(*t)).collect();
    assert_eq!(values, (1..=8).map(|i| i * 10).collect::<Vec<u64>>());
    assert_eq!(times, (1..=8).map(|i| i * 100).collect::<Vec<u64>>());
}

/// A historical `channel` that reaches end-of-stream ends a `RunFor::Forever`
/// run even while another source is still scheduling (#978). The exhausted
/// receiver stops re-arming, but a ticker keeps the kernel alive, so before the
/// fix the run never returned and engine time walked forward until `NanoTime`
/// overflowed.
#[test]
fn channel_eof_ends_a_historical_run_with_another_source() {
    let period = Duration::from_nanos(10);
    let g = GraphBuilder::new();
    let (values, sender) = g.channel::<u64>();
    let ticks = g.ticker(period).count().with_time().accumulate();
    let acc = values.with_time().accumulate();
    let mut r = g.build();

    sender.send_at(1, NanoTime::new(100));
    sender.close();

    r.run(RunMode::HistoricalFrom(NanoTime::ZERO), RunFor::Forever)
        .unwrap();

    let expected: Vec<(NanoTime, u64)> = (0..=10).map(|i| (NanoTime::new(i * 10), i + 1)).collect();
    assert_eq!(
        r.value(&ticks),
        expected,
        "the ticker stops at the channel's last instant, not at an overflow"
    );
    let delivered: Vec<(NanoTime, Vec<u64>)> = r
        .value(&acc)
        .into_iter()
        .map(|(t, b)| (t, b.iter().copied().collect()))
        .collect();
    assert_eq!(
        delivered,
        vec![(NanoTime::new(100), vec![1])],
        "the value buffered at the channel's last instant is still delivered"
    );
}

/// Several channels of different lengths: the run replays the longest and only
/// then does the ticker stop. A single shared "a channel closed" flag would cut
/// this off at the first close and drop the later value — which is why the
/// historical arm waits for every receiver (`merge_all_forwards_bursts_intact`
/// pins the same rule without a second scheduler).
#[test]
fn historical_run_waits_for_every_channel_before_it_ends() {
    let period = Duration::from_nanos(10);
    let g = GraphBuilder::new();
    let (early, early_tx) = g.channel::<u64>();
    let (late, late_tx) = g.channel::<u64>();
    let ticks = g.ticker(period).count().with_time().accumulate();
    let early_out = early.with_time().accumulate();
    let late_out = late.with_time().accumulate();
    let mut r = g.build();

    early_tx.send_at(1, NanoTime::new(10));
    early_tx.close();
    late_tx.send_at(2, NanoTime::new(30));
    late_tx.close();

    r.run(RunMode::HistoricalFrom(NanoTime::ZERO), RunFor::Forever)
        .unwrap();

    assert_eq!(
        r.value(&ticks),
        vec![
            (NanoTime::new(0), 1),
            (NanoTime::new(10), 2),
            (NanoTime::new(20), 3),
            (NanoTime::new(30), 4),
        ],
        "the run outlives the first channel's close, then stops"
    );
    assert_eq!(
        timed(r.value(&early_out)),
        vec![(NanoTime::new(10), vec![1])],
        "the early value lands at its own instant"
    );
    assert_eq!(
        timed(r.value(&late_out)),
        vec![(NanoTime::new(30), vec![2])],
        "the later channel's value still lands after the early one closed"
    );
}

/// A `delay` downstream of the feed is work the feed still drives, so the run
/// must let it fire before ending. Ending at the feed's last instant instead
/// silently drops the delayed copy of its last value.
#[test]
fn pending_delay_drains_before_a_historical_run_ends() {
    let g = GraphBuilder::new();
    let (values, sender) = g.channel::<u64>();
    let delayed = values
        .delay(Duration::from_nanos(50))
        .with_time()
        .accumulate();
    let mut r = g.build();

    sender.send_at(1, NanoTime::new(100));
    sender.close();

    r.run(RunMode::HistoricalFrom(NanoTime::ZERO), RunFor::Forever)
        .unwrap();

    let delivered: Vec<(NanoTime, Vec<u64>)> = r
        .value(&delayed)
        .into_iter()
        .map(|(t, b)| (t, b.iter().copied().collect()))
        .collect();
    assert_eq!(
        delivered,
        vec![(NanoTime::new(150), vec![1])],
        "the delayed value fires after the feed closed"
    );
}

/// The same for a `feedback` edge. The source is scheduled a tick after the
/// forwarder runs rather than reached through a tick edge, so it is the case
/// that would be missed by a rule that only looked at active downstreams.
#[test]
fn pending_feedback_drains_before_a_historical_run_ends() {
    let g = GraphBuilder::new();
    let (values, sender) = g.channel::<u64>();
    let (fed_back, sink) = g.feedback::<u64>();
    let _loop = values
        .collapse()
        .delay(Duration::from_nanos(50))
        .feedback(&sink);
    let out = fed_back.with_time().accumulate();
    let mut r = g.build();

    sender.send_at(1, NanoTime::new(100));
    sender.close();

    r.run(RunMode::HistoricalFrom(NanoTime::ZERO), RunFor::Forever)
        .unwrap();

    assert_eq!(
        r.value(&out),
        vec![(NanoTime::new(151), 1)],
        "the fed-back value still lands after the feed closed"
    );
}

/// An explicit bound is not overridden by feed exhaustion: `RunFor::Duration`
/// owns the stop, which is what lets a bounded backtest keep ticking after its
/// data stops (settlement, funding, a final mark). Same graph as the `Forever`
/// test above, opposite bound.
#[test]
fn explicit_duration_keeps_its_tail_after_the_data_stops() {
    let period = Duration::from_nanos(10);
    let g = GraphBuilder::new();
    let (values, sender) = g.channel::<u64>();
    let ticks = g.ticker(period).count().with_time().accumulate();
    let recv = values.with_time().accumulate();
    let mut r = g.build();

    sender.send_at(1, NanoTime::new(20));
    sender.close();

    r.run(
        RunMode::HistoricalFrom(NanoTime::ZERO),
        RunFor::Duration(Duration::from_nanos(100)),
    )
    .unwrap();

    let expected: Vec<(NanoTime, u64)> = (0..12).map(|i| (NanoTime::new(i * 10), i + 1)).collect();
    assert_eq!(
        r.value(&ticks),
        expected,
        "the ticker runs to the explicit bound, not to the last value"
    );
    assert_eq!(
        timed(r.value(&recv)),
        vec![(NanoTime::new(20), vec![1])],
        "and the data still replays"
    );
}

/// The same bound with the producer's sender dropped instead of closed. A
/// disconnect is an implicit end-of-stream, and it must not end a bounded run
/// early either.
#[test]
fn explicit_duration_keeps_its_tail_for_a_dropped_sender() {
    let period = Duration::from_nanos(10);
    let g = GraphBuilder::new();
    let (values, sender) = g.channel::<u64>();
    let ticks = g.ticker(period).count().with_time().accumulate();
    let recv = values.with_time().accumulate();
    let mut r = g.build();

    sender.send_at(1, NanoTime::new(20));
    drop(sender);

    r.run(
        RunMode::HistoricalFrom(NanoTime::ZERO),
        RunFor::Duration(Duration::from_nanos(100)),
    )
    .unwrap();

    let expected: Vec<(NanoTime, u64)> = (0..12).map(|i| (NanoTime::new(i * 10), i + 1)).collect();
    assert_eq!(
        r.value(&ticks),
        expected,
        "a dropped sender does not end a bounded run early"
    );
    assert_eq!(timed(r.value(&recv)), vec![(NanoTime::new(20), vec![1])]);
}

/// The realtime counterpart: `close()` already ended a `Forever` run through
/// the `Message::EndOfStream` arm, and a ticker scheduling alongside must not
/// change that. Guards the historical fix above from touching the other path.
#[test]
fn channel_eof_ends_a_realtime_run_with_another_source() {
    let g = GraphBuilder::new();
    let (values, sender) = g.channel::<u64>();
    let _ticks = g.ticker(Duration::from_millis(1)).count();
    let acc = values
        .map(|b: &Burst<u64>| b.iter().copied().collect::<Vec<u64>>())
        .accumulate();
    let mut r = g.build();

    sender.send(7);
    sender.close();

    r.run(RunMode::RealTime, RunFor::Forever).unwrap();

    assert_eq!(r.value(&acc), vec![vec![7]]);
    // The run returned while the ticker was still armed; whether it managed a
    // tick before the close is scheduling-dependent, so only the value is pinned.
}

/// A receiver still open when another one drains is not done, so it cannot end
/// the run. The second channel's sender is held open past the first close and
/// then closed from another thread — the only way to end a `Forever` run with a
/// genuinely open feed without hanging the test.
#[test]
fn open_channel_does_not_end_a_historical_run() {
    let g = GraphBuilder::new();
    let (early, early_tx) = g.channel::<u64>();
    let (late, late_tx) = g.channel::<u64>();
    let early_out = early.with_time().accumulate();
    let late_out = late.with_time().accumulate();
    let mut r = g.build();

    early_tx.send_at(1, NanoTime::new(10));
    early_tx.close();
    late_tx.send_at(2, NanoTime::new(30));

    let producer = std::thread::spawn(move || {
        // Long enough that the graph reaches t=30 with this sender still open.
        std::thread::sleep(Duration::from_millis(20));
        late_tx.close();
    });

    r.run(RunMode::HistoricalFrom(NanoTime::ZERO), RunFor::Forever)
        .unwrap();
    producer.join().expect("producer thread");

    assert_eq!(
        timed(r.value(&early_out)),
        vec![(NanoTime::new(10), vec![1])],
        "the early channel's value lands while the late one is still open"
    );
    assert_eq!(
        timed(r.value(&late_out)),
        vec![(NanoTime::new(30), vec![2])],
        "the open channel's later value is replayed, so the first close did not end the run"
    );
}

/// A feed closed before it ever sends is done from the start, so the run must
/// not spin on the ticker either. The receiver still gets one cycle at the start
/// instant to report itself, alongside whatever else is due then.
#[test]
fn empty_closed_channel_ends_a_historical_run() {
    let period = Duration::from_nanos(10);
    let g = GraphBuilder::new();
    let (_values, sender) = g.channel::<u64>();
    let ticks = g.ticker(period).count().with_time().accumulate();
    let mut r = g.build();

    sender.close();

    r.run(RunMode::HistoricalFrom(NanoTime::ZERO), RunFor::Forever)
        .unwrap();

    assert_eq!(
        r.value(&ticks),
        vec![(NanoTime::ZERO, 1)],
        "the run ends on the first cycle, not after an overflow"
    );
}
