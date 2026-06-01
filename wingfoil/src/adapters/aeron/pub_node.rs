//! Aeron publisher node.
//!
//! Serialises values from an upstream stream and offers them to an Aeron
//! publication on every graph cycle.  Only `RunMode::RealTime` is supported.
//!
//! An opt-in **status side-channel** (`AeronPub::aeron_pub_with_status`)
//! returns a paired reactive
//! [`AeronStatusStream`](super::AeronStatusStream) that emits
//! `Connected` / `BackPressured` / `Closed` / `Disconnected` transitions.

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
            let bytes = (self.serialiser)(item);
            match self.backend.offer(&bytes) {
                Ok(()) => {
                    offer_outcome = Some(AeronStatus::Connected);
                }
                Err(e) => {
                    if let Some(TransportError::BackPressure) = e.downcast_ref::<TransportError>() {
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

        // After the loop, derive a status if none was set (empty burst). If at
        // least one offer succeeded we already hold `Connected` in
        // `offer_outcome` — apply it once.
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
        status: Some(status),
    }
    .into_node()
}

// ---------------------------------------------------------------------------
// Public fluent trait
// ---------------------------------------------------------------------------

/// Fluent extension that lets a `Rc<dyn Stream<Burst<T>>>` publish to Aeron.
///
/// Two constructors:
///
/// - [`aeron_pub`](Self::aeron_pub) — offer every burst item each cycle.
/// - [`aeron_pub_with_status`](Self::aeron_pub_with_status) — also return a
///   reactive [`AeronStatusStream`](super::AeronStatusStream).
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
    /// Publish every burst item on every cycle. No status side-channel.
    fn aeron_pub<B: AeronPublisherBackend + 'static>(
        self: &Rc<Self>,
        publisher: B,
        serialiser: impl Fn(&T) -> Vec<u8> + 'static,
    ) -> Rc<dyn Node>;

    /// Publish every burst item and emit lifecycle transitions onto a paired
    /// reactive [`AeronStatusStream`](super::AeronStatusStream).
    fn aeron_pub_with_status<B: AeronPublisherBackend + 'static>(
        self: &Rc<Self>,
        publisher: B,
        serialiser: impl Fn(&T) -> Vec<u8> + 'static,
    ) -> (Rc<dyn Node>, Rc<dyn Stream<Burst<AeronStatus>>>);
}

impl<T: Element> AeronPub<T> for dyn Stream<Burst<T>> {
    fn aeron_pub<B: AeronPublisherBackend + 'static>(
        self: &Rc<Self>,
        publisher: B,
        serialiser: impl Fn(&T) -> Vec<u8> + 'static,
    ) -> Rc<dyn Node> {
        build(self.clone(), serialiser, publisher)
    }

    fn aeron_pub_with_status<B: AeronPublisherBackend + 'static>(
        self: &Rc<Self>,
        publisher: B,
        serialiser: impl Fn(&T) -> Vec<u8> + 'static,
    ) -> (Rc<dyn Node>, Rc<dyn Stream<Burst<AeronStatus>>>) {
        let status = Rc::new(RefCell::new(AeronStatusStream::default()));
        let status_stream: Rc<dyn Stream<Burst<AeronStatus>>> = status.clone();
        let node = build_with_status(self.clone(), serialiser, publisher, Rc::clone(&status));
        // Drive the status node off the publisher's tick (no busy-spin).
        status.borrow_mut().set_producer(Rc::downgrade(&node));
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
        use crate::adapters::aeron::buffer::FragmentBuffer;
        use crate::adapters::aeron::sub_fragment_node::AeronSpinSubFragmentNode;
        use crate::adapters::aeron::transport::MockSubscriber;
        let batches: Vec<Vec<Vec<u8>>> = values
            .into_iter()
            .map(|v| vec![v.to_le_bytes().to_vec()])
            .collect();
        let backend = MockSubscriber::new(batches);
        AeronSpinSubFragmentNode::new(
            backend,
            |f: &FragmentBuffer<'_>| -> Result<Option<i64>, TransportError> {
                Ok(f.as_ref().try_into().ok().map(i64::from_le_bytes))
            },
        )
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
    /// the status tests.
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

    /// Source stream that emits the same `Burst<i64>` every cycle.
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

    // ---------------------------------------------------------------------
    // Baseline publish tests.
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
        let shared = Rc::new(RefCell::new(MockPublisher::new()));
        let stream = make_spin_stream(vec![99i64]);
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
    // Status discipline tests.
    // ---------------------------------------------------------------------

    #[test]
    fn given_aeron_pub_when_offers_every_cycle() {
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
            "aeron_pub MUST call offer() every cycle"
        );
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
