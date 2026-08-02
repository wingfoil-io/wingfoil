//! Real-socket parity tests for the zmq adapter — a port of the core pub/sub
//! tests from the legacy `legacy/wingfoil/src/adapters/zmq/integration_tests.rs`.
//!
//! These need `libzmq` and real TCP sockets, but no external service or
//! container. Because they run against a live wall clock, they assert received
//! *values* (consecutive counters, connection status) rather than exact tick
//! times — the zmq source is realtime-only, so there is no historical timeline
//! to replay deterministically. Run with:
//! ```sh
//! cargo test -p wingfoil-next --features zmq-integration-test \
//!   -- --test-threads=1 --nocapture
//! ```
//!
//! Parity coverage note: `first_message_not_dropped` ports legacy's
//! `zmq_first_message_not_dropped` — publisher and subscriber in one graph, with
//! a delayed publisher, asserting counter value 1 is never lost to the ZMQ
//! slow-joiner. Legacy only asserts first-message-not-dropped in that
//! single-graph layout; its cross-thread test asserts consecutiveness only,
//! which `round_trip_consecutive_counters` covers here. The publisher delay is
//! larger than legacy's 200 ms ([`SUB_SETTLE`]) because next's subscriber runs
//! over the `channel` layer (a background thread feeding the graph) rather than
//! legacy's `ReceiverStream`, and takes longer to connect and propagate its
//! subscription filter after the graph starts.
#![cfg(feature = "zmq-integration-test")]

use std::thread::JoinHandle;
use std::time::Duration;

use wingfoil_next::adapters::zmq::{ZeroMqPub, ZmqStatus, zmq_sub};
use wingfoil_next::prelude::*;
use wingfoil_next::{RunFor, RunMode};

/// Spawn a publisher graph on its own thread: a `count()` published as UTF-8
/// bytes every `period`, for `run_for`.
fn spawn_publisher(
    port: u16,
    period: Duration,
    run_for: Duration,
) -> JoinHandle<anyhow::Result<()>> {
    std::thread::spawn(move || -> anyhow::Result<()> {
        let g = GraphBuilder::new();
        let _sink = g
            .ticker(period)
            .count()
            .map(|n: &u64| format!("{n}").into_bytes())
            .zmq_pub(port, ());
        g.build()
            .run(RunMode::RealTime, RunFor::Duration(run_for))?;
        Ok(())
    })
}

/// Subscribe to `address` for `run_for` and return every received counter value.
fn receive_counters(address: &str, run_for: Duration) -> anyhow::Result<Vec<u64>> {
    let g = GraphBuilder::new();
    let (data, _status) = zmq_sub::<Vec<u8>>(&g, RunMode::RealTime, address)?;
    let received = data.collapse_accumulate();
    let mut runner = g.build();
    runner.run(RunMode::RealTime, RunFor::Duration(run_for))?;
    let values: Vec<Vec<u8>> = runner.value(&received);
    Ok(values
        .into_iter()
        .map(|b| String::from_utf8(b).expect("utf8").parse().expect("u64"))
        .collect())
}

#[test]
fn round_trip_consecutive_counters() {
    let port = 5711;
    let address = format!("tcp://127.0.0.1:{port}");
    let publisher = spawn_publisher(port, Duration::from_millis(50), Duration::from_secs(2));

    let values = receive_counters(&address, Duration::from_millis(1500)).unwrap();
    assert!(values.len() >= 5, "expected >= 5 counters, got {values:?}");
    for w in values.windows(2) {
        assert_eq!(w[1], w[0] + 1, "expected consecutive counters: {values:?}");
    }
    publisher
        .join()
        .expect("publisher thread panicked")
        .unwrap();
}

/// Head-start the publisher gives the subscriber to connect and propagate its
/// subscription filter before the first message is sent. Legacy uses 200 ms
/// with its `ReceiverStream` subscriber; next's `channel`-based subscriber
/// (a background thread feeding the graph) establishes more slowly, so the test
/// allows a wider, machine-safe window. Purely a test settle time — the adapter
/// keeps legacy's 50 ms post-accept flush window unchanged.
///
/// This window is now genuinely effective: `zmq_pub` binds its `PUB` socket at
/// graph `start()` (not lazily on the first publish), so the subscriber connects
/// and its subscription filter propagates *during* this settle time, well before
/// the first payload — the mechanism behind the earlier deviation **A6**
/// flakiness. Kept generous (1500 ms) as CI-load headroom; the guarantee no
/// longer rests on the ~50 ms post-accept margin.
const SUB_SETTLE: Duration = Duration::from_millis(1500);

#[test]
fn first_message_not_dropped() {
    // Faithful port of legacy's `zmq_first_message_not_dropped`: publisher and
    // subscriber share ONE graph so they start together deterministically, and
    // the publisher's output is delayed by `SUB_SETTLE` so the subscriber's
    // filter is live before the first message. That makes the slow-joiner
    // buffering race-free, so counter value 1 is never dropped. Legacy only
    // asserts first-message-not-dropped in this single-graph layout; across
    // *separate* threads (where the publisher can race ahead of the subscriber's
    // connect) it asserts consecutiveness only — which
    // `round_trip_consecutive_counters` covers here.
    let port = 5712;
    let address = format!("tcp://127.0.0.1:{port}");
    let period = Duration::from_millis(50);

    let g = GraphBuilder::new();
    let _sink = g
        .ticker(period)
        .count()
        .delay(SUB_SETTLE)
        .map(|n: &u64| format!("{n}").into_bytes())
        .zmq_pub(port, ());
    let (data, _status) = zmq_sub::<Vec<u8>>(&g, RunMode::RealTime, &address).unwrap();
    let received = data.collapse_accumulate();

    let mut runner = g.build();
    runner
        .run(
            RunMode::RealTime,
            RunFor::Duration(SUB_SETTLE + period * 18),
        )
        .unwrap();
    let values: Vec<u64> = runner
        .value(&received)
        .into_iter()
        .map(|b| String::from_utf8(b).expect("utf8").parse().expect("u64"))
        .collect();
    assert!(!values.is_empty(), "no values received");
    assert_eq!(values[0], 1, "first message dropped: got {values:?}");
}

#[test]
fn first_message_not_dropped_no_delay() {
    // Faithful port of legacy's `zmq_first_message_not_dropped_no_delay` — the
    // sibling of `first_message_not_dropped` with NO artificial startup delay on
    // the publisher. Publisher and subscriber share ONE graph (so they start
    // together) and the publisher begins ticking immediately, isolating the
    // publisher's buffer-until-accept slow-joiner path: it buffers outgoing
    // messages until the first subscriber connects (up to `BUFFER_TIMEOUT`, plus
    // the post-accept subscription-propagation window), so counter value 1 must
    // never be lost even without a settle delay to have the subscription live
    // before the first message. Run generously (2 s) as CI-load headroom for
    // next's `channel`-based subscriber to connect within the buffering window.
    let port = 5716;
    let address = format!("tcp://127.0.0.1:{port}");
    let period = Duration::from_millis(50);

    let g = GraphBuilder::new();
    let _sink = g
        .ticker(period)
        .count()
        .map(|n: &u64| format!("{n}").into_bytes())
        .zmq_pub(port, ());
    let (data, _status) = zmq_sub::<Vec<u8>>(&g, RunMode::RealTime, &address).unwrap();
    let received = data.collapse_accumulate();

    let mut runner = g.build();
    runner
        .run(RunMode::RealTime, RunFor::Duration(Duration::from_secs(2)))
        .unwrap();
    let values: Vec<u64> = runner
        .value(&received)
        .into_iter()
        .map(|b| String::from_utf8(b).expect("utf8").parse().expect("u64"))
        .collect();
    assert!(!values.is_empty(), "no values received");
    assert_eq!(values[0], 1, "first message dropped: got {values:?}");
}

#[test]
fn reports_connected_status() {
    // The subscriber's socket monitor surfaces a `Connected` transition on the
    // status stream — parity of legacy `zmq_reports_connected_status`.
    let port = 5713;
    let address = format!("tcp://127.0.0.1:{port}");
    let publisher = spawn_publisher(port, Duration::from_millis(50), Duration::from_secs(2));

    let g = GraphBuilder::new();
    let (data, status) = zmq_sub::<Vec<u8>>(&g, RunMode::RealTime, &address).unwrap();
    let received = data.collapse_accumulate();
    let statuses = status.accumulate();
    let mut runner = g.build();
    runner
        .run(
            RunMode::RealTime,
            RunFor::Duration(Duration::from_millis(1500)),
        )
        .unwrap();

    assert!(
        !runner.value(&received).is_empty(),
        "no data received alongside status"
    );
    let statuses: Vec<ZmqStatus> = runner.value(&statuses);
    assert!(
        statuses.contains(&ZmqStatus::Connected),
        "expected a Connected status, got: {statuses:?}"
    );
    publisher
        .join()
        .expect("publisher thread panicked")
        .unwrap();
}

#[test]
fn deserialization_error_propagates() {
    // A publisher sending frames the subscriber cannot decode must abort the
    // subscriber's run with an error — parity of legacy
    // `zmq_deserialization_error_propagates`.
    let port = 5714;
    let address = format!("tcp://127.0.0.1:{port}");

    let publisher = std::thread::spawn(move || {
        let ctx = zmq::Context::new();
        let sock = ctx.socket(zmq::PUB).unwrap();
        sock.bind(&format!("tcp://127.0.0.1:{port}")).unwrap();
        std::thread::sleep(Duration::from_millis(200));
        for _ in 0..40 {
            sock.send("not valid bincode".as_bytes(), 0).unwrap();
            std::thread::sleep(Duration::from_millis(50));
        }
    });

    let g = GraphBuilder::new();
    let (data, _status) = zmq_sub::<u64>(&g, RunMode::RealTime, &address).unwrap();
    let _acc = data.collapse_accumulate();
    let result = g
        .build()
        .run(RunMode::RealTime, RunFor::Duration(Duration::from_secs(3)));
    assert!(
        result.is_err(),
        "expected the deserialization error to propagate"
    );
    let _ = publisher.join();
}

#[test]
fn sub_stops_cleanly_without_publisher_endofstream() {
    // A publisher that binds then drops the socket without sending EndOfStream
    // (a crash) must not hang the subscriber; the runner's duration bounds the
    // run and the background thread stops via its stop flag — parity of legacy
    // `zmq_sub_stops_cleanly_without_publisher_endofstream`.
    let port = 5715;
    let address = format!("tcp://127.0.0.1:{port}");

    std::thread::spawn(move || {
        let ctx = zmq::Context::new();
        let sock = ctx.socket(zmq::PUB).unwrap();
        sock.bind(&format!("tcp://127.0.0.1:{port}")).unwrap();
        std::thread::sleep(Duration::from_millis(500));
        // sock drops here — no EndOfStream.
    });

    let g = GraphBuilder::new();
    let (data, _status) = zmq_sub::<u64>(&g, RunMode::RealTime, &address).unwrap();
    let _acc = data.collapse_accumulate();

    let start = std::time::Instant::now();
    g.build()
        .run(
            RunMode::RealTime,
            RunFor::Duration(Duration::from_millis(300)),
        )
        .unwrap();
    let elapsed = start.elapsed();
    assert!(
        elapsed < Duration::from_secs(2),
        "subscriber took too long to stop: {elapsed:?}"
    );
}
