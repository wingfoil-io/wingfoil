//! iceoryx2 adapter tests — the in-process `Local` service variant end-to-end
//! (no shared memory, no subprocesses) plus the wiring-time guards. A parity
//! port of the legacy `legacy/wingfoil/src/adapters/iceoryx2/local_tests.rs`.
//!
//! The cross-process `Ipc` tests live in `tests/iceoryx2_integration.rs` behind
//! the `iceoryx2-integration-test` feature. Run these with:
//! ```sh
//! cargo test -p wingfoil --features iceoryx2
//! ```
#![cfg(feature = "iceoryx2")]

use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;

use iceoryx2::prelude::ZeroCopySend;
use wingfoil::adapters::iceoryx2::{
    Iceoryx2Error, Iceoryx2Mode, Iceoryx2PubOpts, Iceoryx2ServiceVariant, Iceoryx2SinkOps,
    Iceoryx2SliceSinkOps, Iceoryx2SubOpts, iceoryx2_sub_opts, iceoryx2_sub_slice_opts,
    iceoryx2_sub_with,
};
use wingfoil::latency::*;
use wingfoil::prelude::*;
use wingfoil::{NanoTime, RunFor, RunMode};

#[repr(C)]
#[derive(Debug, Clone, Copy, Default, ZeroCopySend, PartialEq)]
struct TestData {
    value: u64,
}

/// Unique per test (pid + counter) so parallel tests never collide on a service.
fn unique_service_name(prefix: &str) -> String {
    static COUNTER: AtomicUsize = AtomicUsize::new(0);
    let n = COUNTER.fetch_add(1, Ordering::Relaxed);
    format!("wingfoil/test/local/{prefix}/{}/{n}", std::process::id())
}

fn local_sub_opts(mode: Iceoryx2Mode) -> Iceoryx2SubOpts {
    Iceoryx2SubOpts {
        variant: Iceoryx2ServiceVariant::Local,
        mode,
        ..Default::default()
    }
}

/// Publish `TestData { value }` every 5 ms into `service_name`, subscribe in
/// `mode`, and return every value the subscriber saw.
fn local_round_trip(service_name: &str, mode: Iceoryx2Mode, value: u64, run_ms: u64) -> Vec<u64> {
    let g = GraphBuilder::new();

    let received =
        iceoryx2_sub_opts::<TestData>(&g, RunMode::RealTime, service_name, local_sub_opts(mode))
            .expect("realtime iceoryx2_sub wires without error")
            .collapse_accumulate();

    let _publisher = g
        .ticker(Duration::from_millis(5))
        .map(move |_: &()| burst![TestData { value }])
        .iceoryx2_pub_with(service_name, Iceoryx2ServiceVariant::Local);

    let mut runner = g.build();
    runner
        .run(
            RunMode::RealTime,
            RunFor::Duration(Duration::from_millis(run_ms)),
        )
        .expect("the local round trip runs");
    runner
        .value(&received)
        .into_iter()
        .map(|d| d.value)
        .collect()
}

#[test]
fn local_spin_round_trip() {
    let name = unique_service_name("spin");
    let values = local_round_trip(&name, Iceoryx2Mode::Spin, 42, 100);
    assert!(!values.is_empty(), "expected to receive samples");
    assert!(values.iter().all(|v| *v == 42));
}

#[test]
fn local_threaded_round_trip() {
    let name = unique_service_name("threaded");
    // Background threads + connection establishment can be slow on loaded CI
    // runners, so this mode gets a longer window (legacy parity).
    let values = local_round_trip(&name, Iceoryx2Mode::Threaded, 99, 500);
    assert!(
        !values.is_empty(),
        "expected to receive samples in threaded mode"
    );
    assert!(values.iter().all(|v| *v == 99));
}

#[test]
fn local_signaled_round_trip() {
    let name = unique_service_name("signaled");
    // Signaled mode depends on the Event service and can need a few retries on
    // startup (legacy parity).
    let values = local_round_trip(&name, Iceoryx2Mode::Signaled, 7, 500);
    assert!(
        !values.is_empty(),
        "expected to receive samples in signaled mode"
    );
    assert!(values.iter().all(|v| *v == 7));
}

/// `history_size` is part of the iceoryx2 publish/subscribe service
/// configuration. If publisher and subscriber disagree, `open_or_create()`
/// errors — surfacing here at run start (wingfoil defers port creation to `start()`,
/// as legacy does) with the service name, variant and contract sizes attached.
#[test]
fn local_service_config_mismatch_fails() {
    let service_name = unique_service_name("mismatch");
    let g = GraphBuilder::new();

    // Start hooks fire in wiring order, so the publisher is wired first: it
    // *creates* the service with `history_size: 5`, and the subscriber's start
    // then fails to open it under the stricter `history_size: 7` contract.
    let _publisher = g
        .ticker(Duration::from_millis(5))
        .map(|_: &()| burst![TestData { value: 1 }])
        .iceoryx2_pub_opts(
            &service_name,
            Iceoryx2PubOpts {
                variant: Iceoryx2ServiceVariant::Local,
                history_size: 5,
            },
        );

    let received = iceoryx2_sub_opts::<TestData>(
        &g,
        RunMode::RealTime,
        &service_name,
        Iceoryx2SubOpts {
            variant: Iceoryx2ServiceVariant::Local,
            mode: Iceoryx2Mode::Spin,
            history_size: 7,
        },
    )
    .expect("wiring succeeds — the contract is only checked at start")
    .collapse_accumulate();
    let _ = received;

    let err = g
        .build()
        .run(
            RunMode::RealTime,
            RunFor::Duration(Duration::from_millis(100)),
        )
        .expect_err("expected the contract mismatch to fail the run");

    let ice_err = err
        .downcast_ref::<Iceoryx2Error>()
        .expect("expected an Iceoryx2Error");
    match ice_err {
        Iceoryx2Error::ServiceOpenFailed {
            service_name: s,
            variant,
            history_size,
            subscriber_max_buffer_size,
            ..
        }
        | Iceoryx2Error::ServiceConfigMismatch {
            service_name: s,
            variant,
            history_size,
            subscriber_max_buffer_size,
            ..
        } => {
            assert_eq!(s, &service_name);
            assert_eq!(variant, &Iceoryx2ServiceVariant::Local);
            assert_eq!(*history_size, 7);
            assert!(*subscriber_max_buffer_size >= 16);
        }
        other => panic!("expected ServiceOpenFailed/ServiceConfigMismatch, got {other:?}"),
    }
}

/// Publish `payload` every 5 ms over the `[u8]` slice API, subscribe in `mode`,
/// and return every slice the subscriber saw.
fn local_slice_round_trip(
    service_name: &str,
    mode: Iceoryx2Mode,
    payload: &'static [u8],
    run_ms: u64,
) -> Vec<Vec<u8>> {
    let g = GraphBuilder::new();

    let received =
        iceoryx2_sub_slice_opts(&g, RunMode::RealTime, service_name, local_sub_opts(mode))
            .expect("realtime iceoryx2_sub_slice wires without error")
            .collapse_accumulate();

    let _publisher = g
        .ticker(Duration::from_millis(5))
        .map(move |_: &()| burst![payload.to_vec()])
        .iceoryx2_pub_slice_with(service_name, Iceoryx2ServiceVariant::Local);

    let mut runner = g.build();
    runner
        .run(
            RunMode::RealTime,
            RunFor::Duration(Duration::from_millis(run_ms)),
        )
        .expect("the local slice round trip runs");
    runner.value(&received)
}

#[test]
fn local_slice_spin_round_trip() {
    let name = unique_service_name("slice/spin");
    let values = local_slice_round_trip(&name, Iceoryx2Mode::Spin, b"abc", 150);
    assert!(!values.is_empty(), "expected slice samples");
    assert!(values.iter().all(|v| v.as_slice() == b"abc"));
}

#[test]
fn local_slice_threaded_round_trip() {
    let name = unique_service_name("slice/threaded");
    let values = local_slice_round_trip(&name, Iceoryx2Mode::Threaded, b"def", 500);
    assert!(
        !values.is_empty(),
        "expected slice samples in threaded mode"
    );
    assert!(values.iter().all(|v| v.as_slice() == b"def"));
}

#[test]
fn local_slice_signaled_round_trip() {
    let name = unique_service_name("slice/signaled");
    let values = local_slice_round_trip(&name, Iceoryx2Mode::Signaled, b"ghi", 500);
    assert!(
        !values.is_empty(),
        "expected slice samples in signaled mode"
    );
    assert!(values.iter().all(|v| v.as_slice() == b"ghi"));
}

// ── Wiring-time guards (wingfoil-specific) ──────────────────────────────────────

/// Every subscriber mode rejects a `HistoricalFrom` run at wiring: an iceoryx2
/// subscription is a live, unbounded source with no historical timeline, and the
/// `Threaded`/`Signaled` modes ride the channel layer, whose historical receiver
/// would block-collect the never-closing producer and deadlock at `start`.
#[test]
fn sub_rejects_historical_mode() {
    for mode in [
        Iceoryx2Mode::Spin,
        Iceoryx2Mode::Threaded,
        Iceoryx2Mode::Signaled,
    ] {
        let g = GraphBuilder::new();
        let err = match iceoryx2_sub_opts::<TestData>(
            &g,
            RunMode::HistoricalFrom(NanoTime::ZERO),
            "wingfoil/test/historical",
            local_sub_opts(mode),
        ) {
            Ok(_) => panic!("HistoricalFrom must be rejected at wiring time"),
            Err(e) => e,
        };
        let msg = format!("{err:#}");
        assert!(msg.contains("iceoryx2_sub"), "names the adapter: {msg}");
        assert!(
            msg.contains("HistoricalFrom") || msg.contains("historical"),
            "explains historical replay is unsupported: {msg}"
        );
    }

    let g = GraphBuilder::new();
    let err = match iceoryx2_sub_slice_opts(
        &g,
        RunMode::HistoricalFrom(NanoTime::ZERO),
        "wingfoil/test/historical-slice",
        local_sub_opts(Iceoryx2Mode::Spin),
    ) {
        Ok(_) => panic!("HistoricalFrom must be rejected at wiring time"),
        Err(e) => e,
    };
    assert!(format!("{err:#}").contains("iceoryx2_sub_slice"));
}

/// An invalid service name fails fast — at run start, where wingfoil (like legacy)
/// creates the ports.
#[test]
fn invalid_service_name_fails_at_start() {
    let g = GraphBuilder::new();
    let sub =
        iceoryx2_sub_with::<TestData>(&g, RunMode::RealTime, "", Iceoryx2ServiceVariant::Local)
            .expect("an empty name is not rejected at wiring — the port is built at start");
    let _ = sub.collapse_accumulate();

    let result = g.build().run(RunMode::RealTime, RunFor::Cycles(1));
    assert!(result.is_err(), "expected invalid service name to error");
}

// ── Latency: end-to-end across an iceoryx2 Local hop ────────────────────────

latency_stages! {
    pub TestLatency {
        produce,
        publish,
        receive,
        ack,
    }
}

#[repr(C)]
#[derive(Debug, Clone, Copy, Default, ZeroCopySend, PartialEq)]
struct TestQuote {
    seq: u64,
}

/// Stamp `produce` and `publish` on the way out, ship a
/// `Traced<TestQuote, TestLatency>` over the iceoryx2 Local hop, stamp `receive`
/// and `ack` on the way in, and confirm all four stamps land in monotonic order
/// — proving the `Traced` wrapper crosses the IPC boundary intact.
#[test]
fn local_latency_round_trip() {
    let service_name = unique_service_name("latency");
    let g = GraphBuilder::new();

    // Subscriber side: stamp receive and ack, collect for assertion.
    let received = iceoryx2_sub_with::<Traced<TestQuote, TestLatency>>(
        &g,
        RunMode::RealTime,
        &service_name,
        Iceoryx2ServiceVariant::Local,
    )
    .expect("realtime iceoryx2_sub wires without error")
    .collapse()
    .stamp::<test_latency::receive>()
    .stamp::<test_latency::ack>()
    .accumulate();

    // Publisher side: build the payload, stamp produce and publish.
    let _publisher = g
        .ticker(Duration::from_millis(5))
        .count()
        .map(|seq: &u64| Traced::<TestQuote, TestLatency>::new(TestQuote { seq: *seq }))
        .stamp::<test_latency::produce>()
        .stamp::<test_latency::publish>()
        .map(|t: &Traced<TestQuote, TestLatency>| burst![*t])
        .iceoryx2_pub_with(&service_name, Iceoryx2ServiceVariant::Local);

    let mut runner = g.build();
    runner
        .run(
            RunMode::RealTime,
            RunFor::Duration(Duration::from_millis(150)),
        )
        .expect("the latency round trip runs");

    let values = runner.value(&received);
    assert!(!values.is_empty(), "expected to receive at least one quote");
    for v in &values {
        let l = &v.latency;
        assert!(l.produce > 0, "produce stamp missing");
        assert!(l.publish >= l.produce, "publish < produce");
        assert!(l.receive >= l.publish, "receive < publish");
        assert!(l.ack >= l.receive, "ack < receive");
    }

    // Aggregating the stamps via LatencyStats produces non-zero counts for every
    // adjacent stage transition.
    let mut stats = LatencyStats::<TestLatency>::new();
    for v in &values {
        stats.observe(&v.latency);
    }
    for i in 1..TestLatency::N {
        assert!(
            stats.stages[i].count > 0,
            "stage {i} never observed a delta"
        );
    }
}
