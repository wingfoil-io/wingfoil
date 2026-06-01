//! Aeron publisher node.
//!
//! Serialises values from an upstream stream and offers them to an Aeron
//! publication on every graph cycle.  Only `RunMode::RealTime` is supported.
//!
//! Story 12.5 adds two opt-in disciplines on top of the existing
//! "publish every burst item" base:
//!
//! - **Dedup** (`AeronPub::aeron_pub_dedup`): skip `offer()` when an upstream
//!   burst item equals the last successfully published value. Latest-wins
//!   retry on back-pressure is preserved — a back-pressured offer does NOT
//!   advance the `last_published` marker, so the next cycle retries (or
//!   replaces with a newer value if the upstream has advanced).
//! - **Status side-channel** (`AeronPub::aeron_pub_with_status` /
//!   `aeron_pub_dedup_with_status`): a paired reactive
//!   [`AeronStatusStream`](super::AeronStatusStream) that emits
//!   `Connected` / `BackPressured` / `Closed` / `Disconnected` transitions.

use crate::adapters::aeron::error::TransportError;
use crate::adapters::aeron::status::AeronStatus;
use crate::adapters::aeron::status_stream::AeronStatusStream;
use crate::adapters::aeron::transport::AeronPublisherBackend;
use crate::{Burst, Element, GraphState, IntoNode, MutableNode, Node, RunMode, Stream, UpStreams};
use std::cell::RefCell;
use std::rc::Rc;

// ---------------------------------------------------------------------------
// Internal node
// ---------------------------------------------------------------------------

struct AeronPubNode<T, S, B>
where
    T: Element,
    S: Fn(&T) -> Vec<u8> + 'static,
    B: AeronPublisherBackend,
{
    upstream: Rc<dyn Stream<Burst<T>>>,
    serialiser: S,
    backend: B,
    last_published: Option<T>,
    /// `true` when consecutive equal upstream items should be coalesced into
    /// a single `offer()`. Always implies `eq_fn` is `Some(_)`; in non-dedup
    /// construction this is `false` and `eq_fn` is `None`, keeping
    /// `T: PartialEq` out of the non-dedup construction path.
    dedup: bool,
    /// Comparator used by the dedup path. `Some` iff `dedup == true`;
    /// captured at construction so `T: PartialEq` is required only by the
    /// dedup constructors, not by the node type itself.
    eq_fn: Option<fn(&T, &T) -> bool>,
    status: Option<Rc<RefCell<AeronStatusStream>>>,
}

impl<T, S, B> AeronPubNode<T, S, B>
where
    T: Element,
    S: Fn(&T) -> Vec<u8> + 'static,
    B: AeronPublisherBackend,
{
    fn record_status(&self, new_status: AeronStatus) {
        if let Some(status) = &self.status {
            status.borrow_mut().record(new_status);
        }
    }
}

impl<T, S, B> MutableNode for AeronPubNode<T, S, B>
where
    T: Element,
    S: Fn(&T) -> Vec<u8> + 'static,
    B: AeronPublisherBackend,
{
    fn cycle(&mut self, _state: &mut GraphState) -> anyhow::Result<bool> {
        // Per-cycle clear contract: the status burst starts each cycle empty
        // so consumers only see transitions produced by this cycle.
        if let Some(status) = &self.status {
            status.borrow_mut().clear();
        }

        // Closed is terminal — surface it before attempting to publish.
        if self.backend.is_closed() {
            self.record_status(AeronStatus::Closed);
            return Ok(true);
        }

        let mut offer_outcome: Option<AeronStatus> = None;
        for item in self.upstream.peek_value().iter() {
            let should_publish = if self.dedup {
                match (&self.last_published, self.eq_fn) {
                    (Some(prev), Some(cmp)) => !cmp(item, prev),
                    _ => true,
                }
            } else {
                true
            };
            if !should_publish {
                continue;
            }
            let bytes = (self.serialiser)(item);
            match self.backend.offer(&bytes) {
                Ok(()) => {
                    if self.dedup {
                        self.last_published = Some(item.clone());
                    }
                    offer_outcome = Some(AeronStatus::Connected);
                }
                Err(e) => {
                    if let Some(TransportError::BackPressure) = e.downcast_ref::<TransportError>() {
                        // Latest-wins retry — do NOT advance `last_published`.
                        // The status record is the only side-effect on this
                        // path; the loop ends via early return so any further
                        // items in the burst are dropped (latest-wins).
                        self.record_status(AeronStatus::BackPressured);
                        return Ok(true);
                    }
                    return Err(e);
                }
            }
        }

        // After the loop, derive a status if none was set (empty burst or all
        // items dedup-skipped). If at least one offer succeeded we already
        // hold `Connected` in `offer_outcome` — apply it once.
        let new_status = offer_outcome.unwrap_or_else(|| {
            if self.backend.is_connected() {
                AeronStatus::Connected
            } else {
                AeronStatus::Disconnected
            }
        });
        self.record_status(new_status);
        Ok(true)
    }

    fn upstreams(&self) -> UpStreams {
        UpStreams::new(vec![self.upstream.clone().as_node()], vec![])
    }

    fn start(&mut self, state: &mut GraphState) -> anyhow::Result<()> {
        if state.run_mode() != RunMode::RealTime {
            anyhow::bail!("Aeron publisher only supports RealTime mode");
        }
        Ok(())
    }
}

// ---------------------------------------------------------------------------
// Internal builders (used by the public trait)
// ---------------------------------------------------------------------------

pub(crate) fn build<T, S, B>(
    upstream: Rc<dyn Stream<Burst<T>>>,
    serialiser: S,
    backend: B,
) -> Rc<dyn Node>
where
    T: Element,
    S: Fn(&T) -> Vec<u8> + 'static,
    B: AeronPublisherBackend,
{
    AeronPubNode {
        upstream,
        serialiser,
        backend,
        last_published: None,
        dedup: false,
        eq_fn: None,
        status: None,
    }
    .into_node()
}

pub(crate) fn build_dedup<T, S, B>(
    upstream: Rc<dyn Stream<Burst<T>>>,
    serialiser: S,
    backend: B,
) -> Rc<dyn Node>
where
    T: Element + PartialEq,
    S: Fn(&T) -> Vec<u8> + 'static,
    B: AeronPublisherBackend,
{
    AeronPubNode {
        upstream,
        serialiser,
        backend,
        last_published: None,
        dedup: true,
        eq_fn: Some(|a, b| a == b),
        status: None,
    }
    .into_node()
}

pub(crate) fn build_with_status<T, S, B>(
    upstream: Rc<dyn Stream<Burst<T>>>,
    serialiser: S,
    backend: B,
    status: Rc<RefCell<AeronStatusStream>>,
) -> Rc<dyn Node>
where
    T: Element,
    S: Fn(&T) -> Vec<u8> + 'static,
    B: AeronPublisherBackend,
{
    AeronPubNode {
        upstream,
        serialiser,
        backend,
        last_published: None,
        dedup: false,
        eq_fn: None,
        status: Some(status),
    }
    .into_node()
}

pub(crate) fn build_dedup_with_status<T, S, B>(
    upstream: Rc<dyn Stream<Burst<T>>>,
    serialiser: S,
    backend: B,
    status: Rc<RefCell<AeronStatusStream>>,
) -> Rc<dyn Node>
where
    T: Element + PartialEq,
    S: Fn(&T) -> Vec<u8> + 'static,
    B: AeronPublisherBackend,
{
    AeronPubNode {
        upstream,
        serialiser,
        backend,
        last_published: None,
        dedup: true,
        eq_fn: Some(|a, b| a == b),
        status: Some(status),
    }
    .into_node()
}

// ---------------------------------------------------------------------------
// Public fluent trait
// ---------------------------------------------------------------------------

/// Fluent extension that lets a `Rc<dyn Stream<Burst<T>>>` publish to Aeron.
///
/// Four constructors mirror aerofoil's `AeronPub` trait, the source of the
/// dedup × status decision matrix:
///
/// | Method                          | Dedup | Status side-channel | `T: PartialEq` bound |
/// |---------------------------------|-------|---------------------|----------------------|
/// | [`aeron_pub`](Self::aeron_pub)                           | OFF | OFF | No  |
/// | [`aeron_pub_dedup`](Self::aeron_pub_dedup)               | ON  | OFF | Yes |
/// | [`aeron_pub_with_status`](Self::aeron_pub_with_status)   | OFF | ON  | No  |
/// | [`aeron_pub_dedup_with_status`](Self::aeron_pub_dedup_with_status) | ON | ON | Yes |
///
/// Splitting at the method level keeps the `PartialEq` bound out of `T`s
/// that can't satisfy it (e.g. types containing function pointers).
///
/// ```ignore
/// let pub_ = handle.publication("aeron:ipc", 1002, Duration::from_secs(5)).unwrap();
/// ticker(Duration::from_millis(100))
///     .count()
///     .map(|n: u64| burst![n])
///     .aeron_pub(pub_, |v: &u64| v.to_le_bytes().to_vec())
///     .run(RunMode::RealTime, RunFor::Forever)
///     .unwrap();
/// ```
pub trait AeronPub<T: Element> {
    /// Publish every burst item on every cycle. No dedup. No status
    /// side-channel. Bit-identical to Story 12.4's behaviour.
    fn aeron_pub<B: AeronPublisherBackend + 'static>(
        self: &Rc<Self>,
        publisher: B,
        serialiser: impl Fn(&T) -> Vec<u8> + 'static,
    ) -> Rc<dyn Node>;

    /// Publish every burst item that differs from the last successfully
    /// published value. Latest-wins retry on back-pressure is preserved.
    /// Requires `T: PartialEq`.
    fn aeron_pub_dedup<B: AeronPublisherBackend + 'static>(
        self: &Rc<Self>,
        publisher: B,
        serialiser: impl Fn(&T) -> Vec<u8> + 'static,
    ) -> Rc<dyn Node>
    where
        T: PartialEq;

    /// Publish every burst item (no dedup) and emit lifecycle transitions
    /// onto a paired reactive [`AeronStatusStream`](super::AeronStatusStream).
    fn aeron_pub_with_status<B: AeronPublisherBackend + 'static>(
        self: &Rc<Self>,
        publisher: B,
        serialiser: impl Fn(&T) -> Vec<u8> + 'static,
    ) -> (Rc<dyn Node>, Rc<dyn Stream<Burst<AeronStatus>>>);

    /// Dedup + status side-channel. Requires `T: PartialEq`.
    fn aeron_pub_dedup_with_status<B: AeronPublisherBackend + 'static>(
        self: &Rc<Self>,
        publisher: B,
        serialiser: impl Fn(&T) -> Vec<u8> + 'static,
    ) -> (Rc<dyn Node>, Rc<dyn Stream<Burst<AeronStatus>>>)
    where
        T: PartialEq;
}

impl<T: Element> AeronPub<T> for dyn Stream<Burst<T>> {
    fn aeron_pub<B: AeronPublisherBackend + 'static>(
        self: &Rc<Self>,
        publisher: B,
        serialiser: impl Fn(&T) -> Vec<u8> + 'static,
    ) -> Rc<dyn Node> {
        build(self.clone(), serialiser, publisher)
    }

    fn aeron_pub_dedup<B: AeronPublisherBackend + 'static>(
        self: &Rc<Self>,
        publisher: B,
        serialiser: impl Fn(&T) -> Vec<u8> + 'static,
    ) -> Rc<dyn Node>
    where
        T: PartialEq,
    {
        build_dedup(self.clone(), serialiser, publisher)
    }

    fn aeron_pub_with_status<B: AeronPublisherBackend + 'static>(
        self: &Rc<Self>,
        publisher: B,
        serialiser: impl Fn(&T) -> Vec<u8> + 'static,
    ) -> (Rc<dyn Node>, Rc<dyn Stream<Burst<AeronStatus>>>) {
        let status = Rc::new(RefCell::new(AeronStatusStream::self_scheduling()));
        let status_stream: Rc<dyn Stream<Burst<AeronStatus>>> = status.clone();
        let node = build_with_status(self.clone(), serialiser, publisher, status);
        (node, status_stream)
    }

    fn aeron_pub_dedup_with_status<B: AeronPublisherBackend + 'static>(
        self: &Rc<Self>,
        publisher: B,
        serialiser: impl Fn(&T) -> Vec<u8> + 'static,
    ) -> (Rc<dyn Node>, Rc<dyn Stream<Burst<AeronStatus>>>)
    where
        T: PartialEq,
    {
        let status = Rc::new(RefCell::new(AeronStatusStream::self_scheduling()));
        let status_stream: Rc<dyn Stream<Burst<AeronStatus>>> = status.clone();
        let node = build_dedup_with_status(self.clone(), serialiser, publisher, status);
        (node, status_stream)
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::adapters::aeron::transport::MockPublisher;
    use crate::{Graph, IntoStream, NanoTime, RunFor, RunMode};
    use std::cell::RefCell;

    fn make_spin_stream(values: Vec<i64>) -> Rc<dyn Stream<Burst<i64>>> {
        use crate::adapters::aeron::sub_spin::AeronSpinSubNode;
        use crate::adapters::aeron::transport::MockSubscriber;
        let batches: Vec<Vec<Vec<u8>>> = values
            .into_iter()
            .map(|v| vec![v.to_le_bytes().to_vec()])
            .collect();
        let backend = MockSubscriber::new(batches);
        AeronSpinSubNode::new(backend, |b: &[u8]| {
            b.try_into().ok().map(i64::from_le_bytes)
        })
        .into_stream()
    }

    /// `MockPublisher` wrapped in `Rc<RefCell<…>>` so we can inspect it after
    /// the graph finishes.
    struct SharedMockPublisher(Rc<RefCell<MockPublisher>>);

    impl AeronPublisherBackend for SharedMockPublisher {
        fn offer(&mut self, buf: &[u8]) -> anyhow::Result<()> {
            self.0.borrow_mut().offer(buf)
        }
    }

    /// Richer mock with controllable back-pressure / connected / closed
    /// state. Wrapped in `Rc<RefCell<…>>` for post-run inspection. Used by
    /// the dedup-and-status tests added in Story 12.5.
    struct RichMockPublisher {
        offered: Vec<Vec<u8>>,
        back_pressure: bool,
        connected: bool,
        closed: bool,
    }

    impl RichMockPublisher {
        fn new() -> Self {
            Self {
                offered: Vec::new(),
                back_pressure: false,
                connected: true,
                closed: false,
            }
        }

        fn with_back_pressure() -> Self {
            Self {
                offered: Vec::new(),
                back_pressure: true,
                connected: true,
                closed: false,
            }
        }

        fn closed() -> Self {
            Self {
                offered: Vec::new(),
                back_pressure: false,
                connected: false,
                closed: true,
            }
        }
    }

    struct SharedRichPublisher(Rc<RefCell<RichMockPublisher>>);

    impl AeronPublisherBackend for SharedRichPublisher {
        fn offer(&mut self, buf: &[u8]) -> anyhow::Result<()> {
            let mut inner = self.0.borrow_mut();
            if inner.back_pressure {
                return Err(TransportError::BackPressure.into());
            }
            inner.offered.push(buf.to_vec());
            Ok(())
        }

        fn is_connected(&self) -> bool {
            self.0.borrow().connected
        }

        fn is_closed(&self) -> bool {
            self.0.borrow().closed
        }
    }

    /// Back-pressure first N offers, then succeed. Mirrors aerofoil's
    /// `FlakeyPublisher` (`aerofoil/src/nodes/publisher.rs:703-736`).
    struct FlakeyPublisher {
        offered: Vec<Vec<u8>>,
        back_pressure_count: usize,
    }

    impl FlakeyPublisher {
        fn new(back_pressure_count: usize) -> Self {
            Self {
                offered: Vec::new(),
                back_pressure_count,
            }
        }
    }

    struct SharedFlakeyPublisher(Rc<RefCell<FlakeyPublisher>>);

    impl AeronPublisherBackend for SharedFlakeyPublisher {
        fn offer(&mut self, buf: &[u8]) -> anyhow::Result<()> {
            let mut inner = self.0.borrow_mut();
            if inner.back_pressure_count > 0 {
                inner.back_pressure_count -= 1;
                return Err(TransportError::BackPressure.into());
            }
            inner.offered.push(buf.to_vec());
            Ok(())
        }

        fn is_connected(&self) -> bool {
            true
        }
    }

    /// Source stream that emits the same `Burst<i64>` every cycle. Used by
    /// the dedup tests, which need a steady upstream value.
    struct ConstantBurstSource {
        burst: Burst<i64>,
    }

    impl MutableNode for ConstantBurstSource {
        fn cycle(&mut self, _state: &mut GraphState) -> anyhow::Result<bool> {
            Ok(true)
        }

        fn start(&mut self, state: &mut GraphState) -> anyhow::Result<()> {
            state.always_callback();
            Ok(())
        }

        fn upstreams(&self) -> UpStreams {
            UpStreams::none()
        }
    }

    impl crate::StreamPeekRef<Burst<i64>> for ConstantBurstSource {
        fn peek_ref(&self) -> &Burst<i64> {
            &self.burst
        }
    }

    fn constant_burst_stream(value: i64) -> Rc<dyn Stream<Burst<i64>>> {
        ConstantBurstSource {
            burst: crate::burst![value],
        }
        .into_stream()
    }

    /// Source stream that emits a different `Burst<i64>` on each cycle from
    /// an internal schedule. The first `schedule[i]` value is emitted on the
    /// `i`-th cycle; once exhausted, the last value is held.
    ///
    /// Used by the dedup tests that need the upstream to advance *within a
    /// single graph run* so the AeronPubNode's `last_published` state persists
    /// across the value transition.
    struct ScheduledBurstSource {
        schedule: Vec<i64>,
        cycle_idx: usize,
        current: Burst<i64>,
    }

    impl MutableNode for ScheduledBurstSource {
        fn cycle(&mut self, _state: &mut GraphState) -> anyhow::Result<bool> {
            let value = self
                .schedule
                .get(self.cycle_idx)
                .copied()
                .unwrap_or_else(|| *self.schedule.last().unwrap_or(&0));
            self.current = crate::burst![value];
            self.cycle_idx += 1;
            Ok(true)
        }

        fn start(&mut self, state: &mut GraphState) -> anyhow::Result<()> {
            state.always_callback();
            Ok(())
        }

        fn upstreams(&self) -> UpStreams {
            UpStreams::none()
        }
    }

    impl crate::StreamPeekRef<Burst<i64>> for ScheduledBurstSource {
        fn peek_ref(&self) -> &Burst<i64> {
            &self.current
        }
    }

    fn scheduled_burst_stream(schedule: Vec<i64>) -> Rc<dyn Stream<Burst<i64>>> {
        let first = *schedule.first().unwrap_or(&0);
        ScheduledBurstSource {
            schedule,
            cycle_idx: 0,
            current: crate::burst![first],
        }
        .into_stream()
    }

    // ---------------------------------------------------------------------
    // Existing tests (Story 12.4 baseline — bit-identical).
    // ---------------------------------------------------------------------

    #[test]
    fn publisher_offers_each_value() {
        let shared = Rc::new(RefCell::new(MockPublisher::new()));
        let upstream = make_spin_stream(vec![10i64, 20]);
        let pub_node = build(
            upstream.clone(),
            |v: &i64| v.to_le_bytes().to_vec(),
            SharedMockPublisher(shared.clone()),
        );
        // Publisher only supports RealTime; use Cycles(2) so it exits quickly.
        Graph::new(
            vec![upstream.as_node(), pub_node],
            RunMode::RealTime,
            RunFor::Cycles(2),
        )
        .run()
        .unwrap();
        let values: Vec<i64> = shared
            .borrow()
            .published
            .iter()
            .map(|b| i64::from_le_bytes(b.as_slice().try_into().unwrap()))
            .collect();
        assert_eq!(values, vec![10i64, 20]);
    }

    #[test]
    fn publisher_historical_mode_fails() {
        let upstream = make_spin_stream(vec![]);
        let pub_node = build(
            upstream.clone(),
            |v: &i64| v.to_le_bytes().to_vec(),
            MockPublisher::new(),
        );
        let result = Graph::new(
            vec![upstream.as_node(), pub_node],
            RunMode::HistoricalFrom(NanoTime::ZERO),
            RunFor::Cycles(1),
        )
        .run();
        assert!(result.is_err());
        let msg = format!("{:?}", result.unwrap_err());
        assert!(
            msg.contains("RealTime"),
            "expected RealTime error, got: {msg}"
        );
    }

    #[test]
    fn aeron_pub_trait_method_wires_correctly() {
        use crate::adapters::aeron::AeronMode;
        use crate::adapters::aeron::aeron_sub;
        use crate::adapters::aeron::transport::MockSubscriber;
        let shared = Rc::new(RefCell::new(MockPublisher::new()));
        let backend = MockSubscriber::new(vec![vec![99i64.to_le_bytes().to_vec()]]);
        let stream = aeron_sub(
            backend,
            |b: &[u8]| b.try_into().ok().map(i64::from_le_bytes),
            AeronMode::Spin,
        );
        let pub_node = stream.aeron_pub(SharedMockPublisher(shared.clone()), |v: &i64| {
            v.to_le_bytes().to_vec()
        });
        Graph::new(
            vec![stream.as_node(), pub_node],
            RunMode::RealTime,
            RunFor::Cycles(1),
        )
        .run()
        .unwrap();
        let values: Vec<i64> = shared
            .borrow()
            .published
            .iter()
            .map(|b| i64::from_le_bytes(b.as_slice().try_into().unwrap()))
            .collect();
        assert_eq!(values, vec![99i64]);
    }

    // ---------------------------------------------------------------------
    // Dedup + status discipline tests (Story 12.5 AC #12).
    // ---------------------------------------------------------------------

    #[test]
    fn given_aeron_pub_dedup_when_upstream_repeats_then_one_offer() {
        let shared = Rc::new(RefCell::new(RichMockPublisher::new()));
        let upstream = constant_burst_stream(42);
        let pub_node = upstream.aeron_pub_dedup(SharedRichPublisher(shared.clone()), |v: &i64| {
            v.to_le_bytes().to_vec()
        });
        Graph::new(
            vec![upstream.as_node(), pub_node],
            RunMode::RealTime,
            RunFor::Cycles(3),
        )
        .run()
        .unwrap();
        assert_eq!(
            shared.borrow().offered.len(),
            1,
            "dedup must collapse consecutive equal values to a single offer"
        );
    }

    #[test]
    fn given_aeron_pub_dedup_when_upstream_value_changes_then_offers_new_value() {
        // Single AeronPubNode (i.e. single `last_published` state) across two
        // cycles whose upstream advances 42 → 100. Cycle 1 publishes 42
        // (last_published: None → Some(42)). Cycle 2's upstream is 100; the
        // dedup comparator sees 100 != 42 and publishes. Combined: [42, 100].
        let shared = Rc::new(RefCell::new(RichMockPublisher::new()));
        let upstream = scheduled_burst_stream(vec![42, 100]);
        let pub_node = upstream.aeron_pub_dedup(SharedRichPublisher(shared.clone()), |v: &i64| {
            v.to_le_bytes().to_vec()
        });
        Graph::new(
            vec![upstream.as_node(), pub_node],
            RunMode::RealTime,
            RunFor::Cycles(2),
        )
        .run()
        .unwrap();
        let values: Vec<i64> = shared
            .borrow()
            .offered
            .iter()
            .map(|b| i64::from_le_bytes(b.as_slice().try_into().unwrap()))
            .collect();
        assert_eq!(values, vec![42i64, 100]);
    }

    #[test]
    fn given_aeron_pub_when_default_no_dedup_then_offers_every_cycle() {
        let shared = Rc::new(RefCell::new(RichMockPublisher::new()));
        let upstream = constant_burst_stream(42);
        let pub_node = upstream.aeron_pub(SharedRichPublisher(shared.clone()), |v: &i64| {
            v.to_le_bytes().to_vec()
        });
        Graph::new(
            vec![upstream.as_node(), pub_node],
            RunMode::RealTime,
            RunFor::Cycles(3),
        )
        .run()
        .unwrap();
        assert_eq!(
            shared.borrow().offered.len(),
            3,
            "aeron_pub MUST call offer() every cycle (dedup default = false)"
        );
    }

    #[test]
    fn given_aeron_pub_dedup_when_first_value_equals_t_default_then_first_offer_happens() {
        // Regression guard against the `last_published: T = T::default()` bug
        // (aerofoil/src/nodes/publisher.rs:664-682). 0i64 == i64::default()
        // must still publish on the first cycle.
        let shared = Rc::new(RefCell::new(RichMockPublisher::new()));
        let upstream = constant_burst_stream(0);
        let pub_node = upstream.aeron_pub_dedup(SharedRichPublisher(shared.clone()), |v: &i64| {
            v.to_le_bytes().to_vec()
        });
        Graph::new(
            vec![upstream.as_node(), pub_node],
            RunMode::RealTime,
            RunFor::Cycles(1),
        )
        .run()
        .unwrap();
        assert_eq!(shared.borrow().offered.len(), 1);
        assert_eq!(shared.borrow().offered[0], 0i64.to_le_bytes().to_vec());
    }

    #[test]
    fn given_aeron_pub_dedup_when_back_pressure_then_retries_same_value() {
        // FlakeyPublisher::new(2) back-pressures the first two offers, then
        // succeeds. With dedup on, the back-pressured offers MUST NOT advance
        // `last_published` — cycle 3's retry of `42` must surface.
        let shared = Rc::new(RefCell::new(FlakeyPublisher::new(2)));
        let upstream = constant_burst_stream(42);
        let pub_node = upstream
            .aeron_pub_dedup(SharedFlakeyPublisher(shared.clone()), |v: &i64| {
                v.to_le_bytes().to_vec()
            });
        Graph::new(
            vec![upstream.as_node(), pub_node],
            RunMode::RealTime,
            RunFor::Cycles(3),
        )
        .run()
        .unwrap();
        let values: Vec<i64> = shared
            .borrow()
            .offered
            .iter()
            .map(|b| i64::from_le_bytes(b.as_slice().try_into().unwrap()))
            .collect();
        assert_eq!(values, vec![42i64]);
    }

    #[test]
    fn given_aeron_pub_dedup_when_back_pressure_and_upstream_advances_then_newer_value_wins() {
        // Single AeronPubNode across 2 cycles with a single publisher's dedup
        // state preserved. Cycle 1: upstream = 42, FlakeyPublisher(1) returns
        // BackPressure (count 1→0). The back-pressure branch records the
        // BackPressured status and does NOT advance `last_published`. Cycle 2:
        // upstream advances to 100, should_publish (last=None) = true, offer
        // succeeds → offered == [100] (42 was dropped — latest-wins).
        let shared = Rc::new(RefCell::new(FlakeyPublisher::new(1)));
        let upstream = scheduled_burst_stream(vec![42, 100]);
        let pub_node = upstream
            .aeron_pub_dedup(SharedFlakeyPublisher(shared.clone()), |v: &i64| {
                v.to_le_bytes().to_vec()
            });
        Graph::new(
            vec![upstream.as_node(), pub_node],
            RunMode::RealTime,
            RunFor::Cycles(2),
        )
        .run()
        .unwrap();
        let values: Vec<i64> = shared
            .borrow()
            .offered
            .iter()
            .map(|b| i64::from_le_bytes(b.as_slice().try_into().unwrap()))
            .collect();
        assert_eq!(values, vec![100i64]);
    }

    #[test]
    fn given_aeron_pub_with_status_when_offer_succeeds_then_status_transitions_to_connected() {
        let shared = Rc::new(RefCell::new(RichMockPublisher::new()));
        let upstream = constant_burst_stream(42);
        let (pub_node, status_stream) = upstream
            .aeron_pub_with_status(SharedRichPublisher(shared.clone()), |v: &i64| {
                v.to_le_bytes().to_vec()
            });
        Graph::new(
            vec![upstream.as_node(), pub_node],
            RunMode::RealTime,
            RunFor::Cycles(1),
        )
        .run()
        .unwrap();
        assert_eq!(
            status_stream.peek_value().last(),
            Some(&AeronStatus::Connected),
        );
    }

    #[test]
    fn given_aeron_pub_with_status_when_back_pressure_then_status_transitions_to_back_pressured() {
        let shared = Rc::new(RefCell::new(RichMockPublisher::with_back_pressure()));
        let upstream = constant_burst_stream(42);
        let (pub_node, status_stream) = upstream
            .aeron_pub_with_status(SharedRichPublisher(shared.clone()), |v: &i64| {
                v.to_le_bytes().to_vec()
            });
        Graph::new(
            vec![upstream.as_node(), pub_node],
            RunMode::RealTime,
            RunFor::Cycles(1),
        )
        .run()
        .unwrap();
        assert_eq!(
            status_stream.peek_value().last(),
            Some(&AeronStatus::BackPressured),
        );
    }

    #[test]
    fn given_aeron_pub_with_status_when_closed_then_status_transitions_to_closed() {
        let shared = Rc::new(RefCell::new(RichMockPublisher::closed()));
        let upstream = constant_burst_stream(42);
        let (pub_node, status_stream) = upstream
            .aeron_pub_with_status(SharedRichPublisher(shared.clone()), |v: &i64| {
                v.to_le_bytes().to_vec()
            });
        Graph::new(
            vec![upstream.as_node(), pub_node],
            RunMode::RealTime,
            RunFor::Cycles(1),
        )
        .run()
        .unwrap();
        assert_eq!(
            status_stream.peek_value().last(),
            Some(&AeronStatus::Closed),
        );
        // Closed path short-circuits BEFORE the offer loop — no offer made.
        assert!(shared.borrow().offered.is_empty());
    }

    #[test]
    fn given_aeron_pub_dedup_with_status_when_offer_succeeds_then_offers_and_records_connected() {
        // Direct unit coverage for the combo method (dedup + status). Single
        // cycle: offer succeeds, dedup state advances, status burst records
        // the Disconnected → Connected transition. The combo method wires both
        // disciplines onto the same AeronPubNode.
        let shared = Rc::new(RefCell::new(RichMockPublisher::new()));
        let upstream = constant_burst_stream(42);
        let (pub_node, status_stream) = upstream
            .aeron_pub_dedup_with_status(SharedRichPublisher(shared.clone()), |v: &i64| {
                v.to_le_bytes().to_vec()
            });
        Graph::new(
            vec![upstream.as_node(), pub_node],
            RunMode::RealTime,
            RunFor::Cycles(1),
        )
        .run()
        .unwrap();
        assert_eq!(
            shared.borrow().offered.len(),
            1,
            "combo: dedup path must offer the first value",
        );
        assert_eq!(
            shared.borrow().offered[0],
            42i64.to_le_bytes().to_vec(),
            "combo: serialised payload must match upstream",
        );
        assert_eq!(
            status_stream.peek_value().last(),
            Some(&AeronStatus::Connected),
            "combo: status side-channel must record the Connected transition",
        );
    }

    #[test]
    fn given_aeron_pub_with_status_when_steady_then_no_re_emission() {
        let shared = Rc::new(RefCell::new(RichMockPublisher::new()));
        let upstream = constant_burst_stream(42);
        let (pub_node, status_stream) = upstream
            .aeron_pub_with_status(SharedRichPublisher(shared.clone()), |v: &i64| {
                v.to_le_bytes().to_vec()
            });
        Graph::new(
            vec![upstream.as_node(), pub_node],
            RunMode::RealTime,
            RunFor::Cycles(2),
        )
        .run()
        .unwrap();
        // After cycle 1 the transition Disconnected→Connected emitted; the
        // status burst held one element. After cycle 2 the producer cleared
        // the burst at cycle start and the re-recorded Connected was
        // suppressed by the transition check → burst is empty.
        assert!(status_stream.peek_value().is_empty());
    }
}
