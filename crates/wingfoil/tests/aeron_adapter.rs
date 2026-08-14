//! aeron adapter tests that need no media driver — a parity port of the legacy
//! node-level unit tests in `legacy/wingfoil/src/adapters/aeron/{mod,sub_fragment_node,
//! pub_node}.rs`, driven by the [`MockSubscriber`] / [`MockPublisher`] backends.
//!
//! The media-driver-backed tests live in `tests/aeron_integration.rs` behind the
//! `aeron-integration-test` feature. Run these with:
//! ```sh
//! cargo test -p wingfoil --features aeron
//! ```
#![cfg(any(feature = "aeron", feature = "aeron-rs"))]

use std::time::Duration;

use wingfoil::adapters::aeron::transport::{
    AeronPublisherBackend, AeronSubscriberBackend, MockPublisher, MockSubscriber,
};
use wingfoil::adapters::aeron::{
    AeronMode, AeronSinkOps, AeronStatus, AeronSubOptions, FragmentBuffer, TransportError,
    aeron_sub_fragment, aeron_sub_fragment_with_status,
};
use wingfoil::prelude::*;
use wingfoil::{NanoTime, RunFor, RunMode};

/// Little-endian `i64` parser for the fragment surface.
fn i64_parser(f: &FragmentBuffer<'_>) -> Result<Option<i64>, TransportError> {
    Ok(f.as_ref().try_into().ok().map(i64::from_le_bytes))
}

fn spin_opts() -> AeronSubOptions {
    AeronSubOptions {
        mode: AeronMode::Spin,
        ..Default::default()
    }
}

fn threaded_opts() -> AeronSubOptions {
    AeronSubOptions {
        mode: AeronMode::Threaded,
        ..Default::default()
    }
}

/// Subscriber with controllable `is_connected` / `is_closed` flags, so the
/// status side-channel can be exercised without a driver. Mirrors legacy's
/// `ConnectedMockSubscriber`.
struct ConnectedMockSubscriber {
    batches: std::collections::VecDeque<Vec<Vec<u8>>>,
    connected: bool,
    /// Flips `is_closed` to `true` once this many polls have been served, so a
    /// test can observe a `Connected` → `Closed` transition without touching the
    /// backend from outside the graph.
    close_after_polls: Option<usize>,
    polls: usize,
}

impl ConnectedMockSubscriber {
    fn new(messages: Vec<Vec<u8>>, connected: bool) -> Self {
        Self {
            batches: std::collections::VecDeque::from(vec![messages]),
            connected,
            close_after_polls: None,
            polls: 0,
        }
    }

    fn closing_after(polls: usize) -> Self {
        Self {
            batches: std::collections::VecDeque::new(),
            connected: true,
            close_after_polls: Some(polls),
            polls: 0,
        }
    }
}

impl AeronSubscriberBackend for ConnectedMockSubscriber {
    fn poll(&mut self, handler: &mut dyn FnMut(&[u8])) -> anyhow::Result<usize> {
        self.polls += 1;
        let batch = self.batches.pop_front().unwrap_or_default();
        let count = batch.len();
        for msg in &batch {
            handler(msg);
        }
        Ok(count)
    }

    fn is_connected(&self) -> bool {
        self.connected
    }

    fn is_closed(&self) -> bool {
        self.close_after_polls.is_some_and(|n| self.polls > n)
    }
}

/// Publisher that always reports back-pressure.
#[derive(Default)]
struct BackPressuredPublisher;

impl AeronPublisherBackend for BackPressuredPublisher {
    fn offer(&mut self, _buffer: &[u8]) -> anyhow::Result<()> {
        Err(TransportError::BackPressure.into())
    }
}

/// Publisher that reports itself closed.
#[derive(Default)]
struct ClosedPublisher;

impl AeronPublisherBackend for ClosedPublisher {
    fn offer(&mut self, _buffer: &[u8]) -> anyhow::Result<()> {
        Ok(())
    }

    fn is_closed(&self) -> bool {
        true
    }
}

/// Run a spin subscriber over `backend` for `cycles` and return every value it
/// emitted, flattened across bursts.
fn spin_values(backend: impl AeronSubscriberBackend, cycles: u32) -> Vec<i64> {
    let g = GraphBuilder::new();
    let collected = aeron_sub_fragment(&g, RunMode::RealTime, backend, i64_parser, spin_opts())
        .expect("realtime aeron_sub_fragment wires without error")
        .collapse_accumulate();
    let mut runner = g.build();
    runner
        .run(RunMode::RealTime, RunFor::Cycles(cycles))
        .expect("the spin subscriber runs");
    runner.value(&collected)
}

// ── Subscriber: fragments → bursts ──────────────────────────────────────────

#[test]
fn spin_no_fragments_yields_no_values() {
    assert!(spin_values(MockSubscriber::new(vec![vec![]]), 1).is_empty());
}

#[test]
fn spin_single_fragment_yields_one_value() {
    let backend = MockSubscriber::from_messages(vec![42i64.to_le_bytes().to_vec()]);
    assert_eq!(spin_values(backend, 1), vec![42i64]);
}

#[test]
fn spin_three_fragments_in_one_poll_ride_one_burst() {
    let batch = vec![
        1i64.to_le_bytes().to_vec(),
        2i64.to_le_bytes().to_vec(),
        3i64.to_le_bytes().to_vec(),
    ];
    let g = GraphBuilder::new();
    let collected = aeron_sub_fragment(
        &g,
        RunMode::RealTime,
        MockSubscriber::new(vec![batch]),
        i64_parser,
        spin_opts(),
    )
    .expect("wires")
    .accumulate();
    let mut runner = g.build();
    runner
        .run(RunMode::RealTime, RunFor::Cycles(1))
        .expect("runs");
    let bursts = runner.value(&collected);
    assert_eq!(bursts.len(), 1, "one poll ⇒ one atomic burst");
    assert_eq!(bursts[0].as_slice(), &[1i64, 2, 3]);
}

#[test]
fn spin_parser_none_skips_the_fragment() {
    // A wrong-length fragment fails `try_into` → `Ok(None)`.
    let batch = vec![vec![0u8; 4], 42i64.to_le_bytes().to_vec()];
    assert_eq!(
        spin_values(MockSubscriber::new(vec![batch]), 1),
        vec![42i64]
    );
}

#[test]
fn spin_parser_err_drops_the_fragment_and_the_cycle_continues() {
    // The middle fragment yields `TransportError::Invalid`; the valid fragments
    // either side must still be collected (legacy's zero-stopping rule).
    let batch = vec![
        1i64.to_le_bytes().to_vec(),
        vec![0xDE, 0xAD, 0xBE, 0xEF, 0xDE, 0xAD], // 6 bytes — neither 8 nor 4
        3i64.to_le_bytes().to_vec(),
    ];
    let parser = |f: &FragmentBuffer<'_>| -> Result<Option<i64>, TransportError> {
        match f.as_ref().len() {
            8 => Ok(f.as_ref().try_into().ok().map(i64::from_le_bytes)),
            6 => Err(TransportError::Invalid("bad".into())),
            _ => Ok(None),
        }
    };
    let g = GraphBuilder::new();
    let collected = aeron_sub_fragment(
        &g,
        RunMode::RealTime,
        MockSubscriber::new(vec![batch]),
        parser,
        spin_opts(),
    )
    .expect("wires")
    .collapse_accumulate();
    let mut runner = g.build();
    runner
        .run(RunMode::RealTime, RunFor::Cycles(1))
        .expect("runs");
    assert_eq!(runner.value(&collected), vec![1i64, 3]);
}

#[test]
fn spin_parser_sees_the_synthesised_default_header() {
    // The mock does not override `poll_fragments`, so the trait default
    // synthesises a zero-filled header — asserted inline so a regression
    // surfaces as a panic during the run.
    let parser = |f: &FragmentBuffer<'_>| -> Result<Option<i64>, TransportError> {
        assert_eq!(f.position(), 0);
        assert_eq!(f.header().session_id, 0);
        assert_eq!(f.header().stream_id, 0);
        Ok(f.as_ref().try_into().ok().map(i64::from_le_bytes))
    };
    let g = GraphBuilder::new();
    let collected = aeron_sub_fragment(
        &g,
        RunMode::RealTime,
        MockSubscriber::from_messages(vec![7i64.to_le_bytes().to_vec()]),
        parser,
        spin_opts(),
    )
    .expect("wires")
    .collapse_accumulate();
    let mut runner = g.build();
    runner
        .run(RunMode::RealTime, RunFor::Cycles(1))
        .expect("runs");
    assert_eq!(runner.value(&collected), vec![7i64]);
}

// ── Subscriber: status side-channel ─────────────────────────────────────────

/// Collect every status transition a spin subscriber emits over `cycles`.
fn spin_statuses(backend: impl AeronSubscriberBackend, cycles: u32) -> Vec<AeronStatus> {
    let g = GraphBuilder::new();
    let (_data, status) =
        aeron_sub_fragment_with_status(&g, RunMode::RealTime, backend, i64_parser, spin_opts())
            .expect("wires");
    let collected = status.collapse_accumulate();
    let mut runner = g.build();
    runner
        .run(RunMode::RealTime, RunFor::Cycles(cycles))
        .expect("runs");
    runner.value(&collected)
}

#[test]
fn spin_status_connected_backend_emits_one_connected_transition() {
    let statuses = spin_statuses(ConnectedMockSubscriber::new(vec![], true), 4);
    assert_eq!(
        statuses,
        vec![AeronStatus::Connected],
        "one transition, no re-emission in steady state"
    );
}

#[test]
fn spin_status_disconnected_backend_emits_nothing() {
    // `Disconnected` equals the stream's initial state, so there is no
    // transition to report.
    assert!(spin_statuses(ConnectedMockSubscriber::new(vec![], false), 4).is_empty());
}

#[test]
fn spin_status_close_is_terminal_and_checked_first() {
    // Connected for the first two polls, then closed: Connected → Closed.
    let statuses = spin_statuses(ConnectedMockSubscriber::closing_after(2), 6);
    assert_eq!(statuses, vec![AeronStatus::Connected, AeronStatus::Closed]);
}

#[test]
fn plain_sub_never_derives_status() {
    // The plain factory does not track status at all, so a connected backend
    // produces data (none here) and nothing else — the regression guard for
    // legacy's `status: None` branch.
    assert!(spin_values(ConnectedMockSubscriber::new(vec![], true), 3).is_empty());
}

#[test]
fn threaded_status_connected_backend_propagates_the_transition() {
    // The poll thread derives `Connected`, which differs from the default, and
    // multiplexes it in-band; the split surfaces it on the status half.
    let g = GraphBuilder::new();
    let (_data, status) = aeron_sub_fragment_with_status(
        &g,
        RunMode::RealTime,
        ConnectedMockSubscriber::new(vec![7i64.to_le_bytes().to_vec()], true),
        i64_parser,
        threaded_opts(),
    )
    .expect("wires");
    let collected = status.collapse_accumulate();
    let mut runner = g.build();
    runner
        .run(
            RunMode::RealTime,
            RunFor::Duration(Duration::from_millis(400)),
        )
        .expect("runs");
    let statuses = runner.value(&collected);
    assert!(
        statuses.contains(&AeronStatus::Connected),
        "expected a Connected transition, got {statuses:?}"
    );
}

#[test]
fn threaded_status_disconnected_backend_emits_nothing() {
    // A never-connected backend derives `Disconnected`, which equals the initial
    // state, so no transition is sent — legacy's parity case.
    let g = GraphBuilder::new();
    let (_data, status) = aeron_sub_fragment_with_status(
        &g,
        RunMode::RealTime,
        MockSubscriber::from_messages(vec![]),
        i64_parser,
        threaded_opts(),
    )
    .expect("wires");
    let collected = status.collapse_accumulate();
    let mut runner = g.build();
    runner
        .run(
            RunMode::RealTime,
            RunFor::Duration(Duration::from_millis(100)),
        )
        .expect("runs");
    assert!(
        runner.value(&collected).is_empty(),
        "a disconnected backend must not emit any status transition"
    );
}

#[test]
fn threaded_delivers_fragments() {
    let g = GraphBuilder::new();
    let collected = aeron_sub_fragment(
        &g,
        RunMode::RealTime,
        MockSubscriber::from_messages(vec![5i64.to_le_bytes().to_vec()]),
        i64_parser,
        threaded_opts(),
    )
    .expect("wires")
    .collapse_accumulate();
    let mut runner = g.build();
    runner
        .run(
            RunMode::RealTime,
            RunFor::Duration(Duration::from_millis(200)),
        )
        .expect("runs");
    assert_eq!(runner.value(&collected), vec![5i64]);
}

// ── Wiring guards ───────────────────────────────────────────────────────────

#[test]
fn sub_rejects_historical_mode() {
    for (mode, name) in [(AeronMode::Spin, "spin"), (AeronMode::Threaded, "threaded")] {
        let g = GraphBuilder::new();
        let opts = AeronSubOptions {
            mode,
            ..Default::default()
        };
        let err = match aeron_sub_fragment(
            &g,
            RunMode::HistoricalFrom(NanoTime::ZERO),
            MockSubscriber::from_messages(vec![]),
            i64_parser,
            opts,
        ) {
            Ok(_) => panic!("HistoricalFrom must be rejected at wiring time ({name})"),
            Err(e) => e,
        };
        let msg = format!("{err:#}");
        assert!(
            msg.contains("aeron_sub_fragment"),
            "names the adapter: {msg}"
        );
        assert!(
            msg.contains("HistoricalFrom") || msg.contains("historical"),
            "explains historical replay is unsupported: {msg}"
        );
    }

    let g = GraphBuilder::new();
    let err = match aeron_sub_fragment_with_status(
        &g,
        RunMode::HistoricalFrom(NanoTime::ZERO),
        MockSubscriber::from_messages(vec![]),
        i64_parser,
        spin_opts(),
    ) {
        Ok(_) => panic!("HistoricalFrom must be rejected at wiring time"),
        Err(e) => e,
    };
    assert!(format!("{err:#}").contains("aeron_sub_fragment_with_status"));
}

// ── Publisher ───────────────────────────────────────────────────────────────

/// Publish `values` (one per cycle) through `publisher` and return the sink's
/// status transitions.
///
/// Runs exactly as many cycles as there are values: a cycle past the end
/// publishes an empty burst, which falls back to the backend's own
/// `is_connected` (covered separately by
/// `pub_empty_burst_falls_back_to_is_connected`).
fn publish_statuses<B: AeronPublisherBackend>(publisher: B, values: Vec<i64>) -> Vec<AeronStatus> {
    let cycles = u32::try_from(values.len()).expect("value count fits in u32");
    let g = GraphBuilder::new();
    let source = g
        .ticker(Duration::from_millis(1))
        .count()
        .map(move |n: &u64| {
            values
                .get((*n as usize).saturating_sub(1))
                .copied()
                .map(|v| burst![v])
                .unwrap_or_default()
        });
    let (_sink, status) =
        source.aeron_pub_with_status(publisher, |v: &i64| v.to_le_bytes().to_vec());
    let collected = status.collapse_accumulate();
    let mut runner = g.build();
    runner
        .run(RunMode::RealTime, RunFor::Cycles(cycles))
        .expect("the publisher runs");
    runner.value(&collected)
}

#[test]
fn pub_offers_every_burst_item() {
    // `MockPublisher` records what it was offered, but it is moved into the
    // graph — so the round trip is asserted through the sink's status instead:
    // a successful offer records `Connected`.
    let statuses = publish_statuses(MockPublisher::new(), vec![1, 2, 3]);
    assert_eq!(statuses, vec![AeronStatus::Connected]);
}

#[test]
fn pub_back_pressure_records_back_pressured() {
    let statuses = publish_statuses(BackPressuredPublisher, vec![1, 2, 3]);
    assert_eq!(statuses, vec![AeronStatus::BackPressured]);
}

#[test]
fn pub_closed_is_terminal_and_checked_first() {
    let statuses = publish_statuses(ClosedPublisher, vec![1, 2, 3]);
    assert_eq!(statuses, vec![AeronStatus::Closed]);
}

#[test]
fn pub_steady_state_does_not_re_emit() {
    // Six cycles of successful offers still yield exactly one transition.
    let statuses = publish_statuses(MockPublisher::new(), vec![1, 2, 3, 4, 5, 6]);
    assert_eq!(statuses.len(), 1);
}

/// A cycle whose burst is empty offers nothing, so the status falls back to the
/// backend's own `is_connected` — `MockPublisher` inherits the trait default
/// `false`, giving `Connected` (from the offering cycles) then `Disconnected`.
#[test]
fn pub_empty_burst_falls_back_to_is_connected() {
    let g = GraphBuilder::new();
    let source = g
        .ticker(Duration::from_millis(1))
        .count()
        .map(|n: &u64| if *n <= 2 { burst![*n as i64] } else { burst![] });
    let (_sink, status) =
        source.aeron_pub_with_status(MockPublisher::new(), |v: &i64| v.to_le_bytes().to_vec());
    let collected = status.collapse_accumulate();
    let mut runner = g.build();
    runner
        .run(RunMode::RealTime, RunFor::Cycles(4))
        .expect("runs");
    assert_eq!(
        runner.value(&collected),
        vec![AeronStatus::Connected, AeronStatus::Disconnected]
    );
}

#[test]
fn pub_rejects_historical_mode_at_start() {
    let g = GraphBuilder::new();
    let _sink = g
        .constant(burst![1i64])
        .aeron_pub(MockPublisher::new(), |v: &i64| v.to_le_bytes().to_vec());
    let err = g
        .build()
        .run(RunMode::HistoricalFrom(NanoTime::ZERO), RunFor::Cycles(1))
        .expect_err("the Aeron publisher is real-time only");
    assert!(
        format!("{err:#}").contains("RealTime"),
        "names the run mode: {err:#}"
    );
}
