//! Wire-compatibility tests between the two engines: a **legacy** publisher
//! feeding a **next** subscriber, and the reverse, over real ZMQ sockets.
//!
//! This is the behavioural half of the `WireMessage` wire contract. The unit
//! test `adapters::zmq::tests::wire_format_matches_legacy_message` pins next's
//! own encoding against golden bytes; only this file proves the legacy encoder
//! actually agrees, because it puts a real legacy socket at the other end.
//! Together they close deviation-register **C2** (cutover-plan row 2.3).
//!
//! Both engines run in one test binary: `wingfoil` is already a dev-dependency
//! of `wingfoil-next` (the parity oracle for `tests/engine_semantics.rs`), and
//! the `zmq-cross-engine-test` feature turns on its zmq adapter alongside
//! next's. That is also why this file cannot live in the legacy tree — the
//! dependency edge only runs legacy → next.
//!
//! ```sh
//! cargo test -p wingfoil-next --features zmq-cross-engine-test \
//!   -- --test-threads=1 --nocapture
//! ```
//!
//! **This whole file retires with the legacy tree.** Its job is to keep the two
//! engines interoperable *during* the cutover, so a deployment can move process
//! by process instead of all at once.
//!
//! Ports 5731–5732 (see the port table in `src/adapters/zmq/CLAUDE.md`).
#![cfg(feature = "zmq-cross-engine-test")]

use std::thread::JoinHandle;
use std::time::Duration;

use wingfoil_next::adapters::zmq::{ZeroMqPub, zmq_sub};
use wingfoil_next::prelude::*;
use wingfoil_next::{RunFor, RunMode};

/// How long a publisher runs — longer than the subscriber's window, so the
/// subscriber never starves waiting on a publisher that has already stopped.
const PUB_RUN: Duration = Duration::from_secs(3);
/// How long a subscriber collects for.
const SUB_RUN: Duration = Duration::from_millis(1500);
/// Publish period. Matches legacy's cross-language tests.
const TICK: Duration = Duration::from_millis(50);
/// Head start for the publisher to bind and the subscriber to propagate its
/// subscription filter. ZMQ's slow-joiner problem is why this exists: a `SUB`
/// socket that connects after the first publish silently misses it. Sized like
/// `zmq_integration.rs`'s `SUB_SETTLE` for the same reason — next's subscriber
/// establishes over the `channel` layer and is slower than legacy's
/// `ReceiverStream`.
const BIND_SETTLE: Duration = Duration::from_millis(1500);

/// The assertion both directions share: enough messages arrived, and they are a
/// consecutive counter run with nothing dropped, duplicated or reordered.
///
/// Deliberately not anchored at zero. The subscriber joins mid-stream, so where
/// the run starts is a timing artifact of the settle window; consecutiveness is
/// the real invariant, and it is what a decoder disagreeing about the envelope
/// would break.
fn assert_consecutive(values: &[u64], label: &str) {
    assert!(
        values.len() >= 3,
        "{label}: expected >= 3 messages, got {} ({values:?})",
        values.len()
    );
    for w in values.windows(2) {
        assert_eq!(
            w[1],
            w[0] + 1,
            "{label}: non-consecutive counters {} then {} (full run: {values:?})",
            w[0],
            w[1]
        );
    }
}

/// Parse the UTF-8 counter payload both publishers send.
fn parse_counter(bytes: Vec<u8>, label: &str) -> u64 {
    let text = String::from_utf8(bytes)
        .unwrap_or_else(|e| panic!("{label}: payload is not valid utf8: {e}"));
    text.parse()
        .unwrap_or_else(|e| panic!("{label}: payload {text:?} is not a counter: {e}"))
}

/// A **legacy** publisher: counter bytes on `port`, wired with the legacy
/// engine's `MutableNode` API and encoded by its `channel::Message`.
fn spawn_legacy_publisher(port: u16) -> JoinHandle<anyhow::Result<()>> {
    std::thread::spawn(move || {
        use wingfoil::adapters::zmq::ZeroMqPub as LegacyPub;
        use wingfoil::{
            NodeOperators, RunFor as LegacyRunFor, RunMode as LegacyRunMode, StreamOperators,
            ticker,
        };

        ticker(TICK)
            .count()
            .map(|n: u64| format!("{n}").into_bytes())
            .zmq_pub(port, ())
            .run(LegacyRunMode::RealTime, LegacyRunFor::Duration(PUB_RUN))
    })
}

/// A **next** publisher: the same counter bytes on `port`, through next's
/// `Op`-pattern wiring and its `WireMessage` encoder.
fn spawn_next_publisher(port: u16) -> JoinHandle<anyhow::Result<()>> {
    std::thread::spawn(move || {
        let g = GraphBuilder::new();
        let _sink = g
            .ticker(TICK)
            .count()
            .map(|n: &u64| format!("{n}").into_bytes())
            .zmq_pub(port, ());
        g.build().run(RunMode::RealTime, RunFor::Duration(PUB_RUN))
    })
}

/// A legacy graph writes, a next graph reads. The direction that lets a ported
/// process keep consuming feeds that have not moved yet.
#[test]
fn legacy_pub_to_next_sub() {
    _ = env_logger::try_init();
    let port = 5731u16;
    let address = format!("tcp://127.0.0.1:{port}");

    let publisher = spawn_legacy_publisher(port);
    std::thread::sleep(BIND_SETTLE);

    let g = GraphBuilder::new();
    let (data, _status) =
        zmq_sub::<Vec<u8>>(&g, RunMode::RealTime, address.as_str()).expect("next zmq_sub failed");
    let received = data.collapse_accumulate();
    let mut runner = g.build();
    runner
        .run(RunMode::RealTime, RunFor::Duration(SUB_RUN))
        .expect("next subscriber run failed");

    let values: Vec<u64> = runner
        .value(&received)
        .into_iter()
        .map(|b| parse_counter(b, "legacy_pub_to_next_sub"))
        .collect();

    assert_consecutive(&values, "legacy_pub_to_next_sub");
    publisher
        .join()
        .expect("legacy publisher thread panicked")
        .expect("legacy publisher run failed");
}

/// A next graph writes, a legacy graph reads. The direction that matters for a
/// staged rollout: porting a producer must not break consumers still on legacy.
#[test]
fn next_pub_to_legacy_sub() {
    _ = env_logger::try_init();
    let port = 5732u16;
    let address = format!("tcp://127.0.0.1:{port}");

    let publisher = spawn_next_publisher(port);
    std::thread::sleep(BIND_SETTLE);

    {
        use wingfoil::adapters::zmq::zmq_sub as legacy_zmq_sub;
        use wingfoil::{
            NodeOperators, RunFor as LegacyRunFor, RunMode as LegacyRunMode, StreamOperators,
        };

        let (data, _status) = legacy_zmq_sub::<Vec<u8>>(&address).expect("legacy zmq_sub failed");
        // Assert inside `finally` — legacy's own cross-language test does the
        // same, and it is the one place the collected result is in hand.
        let recv_node = data.collect().finally(|res, _| {
            let values: Vec<u64> = res
                .into_iter()
                .flat_map(|burst| burst.value)
                .map(|b| parse_counter(b, "next_pub_to_legacy_sub"))
                .collect();
            assert_consecutive(&values, "next_pub_to_legacy_sub");
            Ok(())
        });
        recv_node
            .run(LegacyRunMode::RealTime, LegacyRunFor::Duration(SUB_RUN))
            .expect("legacy subscriber run failed");
    }

    publisher
        .join()
        .expect("next publisher thread panicked")
        .expect("next publisher run failed");
}
