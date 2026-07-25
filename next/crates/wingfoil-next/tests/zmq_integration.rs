//! Real-socket parity tests for the zmq adapter — a port of the core pub/sub
//! tests from the classic `wingfoil/src/adapters/zmq/integration_tests.rs`.
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
//! Parity coverage note: classic's `zmq_same_thread` (publisher + subscriber
//! nodes in one graph on one thread) is not ported separately — next's
//! subscriber runs its own background thread over the `channel`, and
//! `round_trip_consecutive_counters` exercises the same pub→sub path across
//! threads. Classic's two first-message variants collapse into one here:
//! `first_message_not_dropped` uses a *no-delay* publisher (the harder
//! slow-joiner case, classic's `..._no_delay`), which subsumes the delayed one.
#![cfg(feature = "zmq-integration-test")]

use std::thread::JoinHandle;
use std::time::Duration;

use wingfoil::{RunFor, RunMode};
use wingfoil_next::adapters::zmq::{ZeroMqPub, ZmqStatus, zmq_sub};
use wingfoil_next::prelude::*;

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

#[test]
fn first_message_not_dropped() {
    // The publisher buffers early messages until the subscriber's subscription
    // filter propagates, so counter value 1 is never lost to the ZMQ
    // slow-joiner problem — parity of classic `zmq_first_message_not_dropped`.
    let port = 5712;
    let address = format!("tcp://127.0.0.1:{port}");
    let publisher = spawn_publisher(port, Duration::from_millis(50), Duration::from_secs(2));

    let values = receive_counters(&address, Duration::from_millis(1500)).unwrap();
    assert!(!values.is_empty(), "no values received");
    assert_eq!(values[0], 1, "first message dropped: got {values:?}");
    publisher
        .join()
        .expect("publisher thread panicked")
        .unwrap();
}

#[test]
fn reports_connected_status() {
    // The subscriber's socket monitor surfaces a `Connected` transition on the
    // status stream — parity of classic `zmq_reports_connected_status`.
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
    // subscriber's run with an error — parity of classic
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
    // run and the background thread stops via its stop flag — parity of classic
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
