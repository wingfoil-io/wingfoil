//! Aeron publisher node.
//!
//! Serialises values from an upstream stream and offers them to an Aeron
//! publication on every graph cycle.  Only `RunMode::RealTime` is supported.

use crate::adapters::aeron::transport::AeronPublisherBackend;
use crate::{Burst, Element, GraphState, IntoNode, MutableNode, Node, RunMode, Stream, UpStreams};
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
}

impl<T, S, B> MutableNode for AeronPubNode<T, S, B>
where
    T: Element,
    S: Fn(&T) -> Vec<u8> + 'static,
    B: AeronPublisherBackend,
{
    fn cycle(&mut self, _state: &mut GraphState) -> anyhow::Result<bool> {
        for item in self.upstream.peek_value().iter() {
            self.backend.offer(&(self.serialiser)(item))?;
        }
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
// Internal builder (used by the public trait)
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
    }
    .into_node()
}

// ---------------------------------------------------------------------------
// Public fluent trait
// ---------------------------------------------------------------------------

/// Fluent extension that lets a `Rc<dyn Stream<Burst<T>>>` publish to Aeron.
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
    fn aeron_pub<B: AeronPublisherBackend + 'static>(
        self: &Rc<Self>,
        publisher: B,
        serialiser: impl Fn(&T) -> Vec<u8> + 'static,
    ) -> Rc<dyn Node>;
}

impl<T: Element> AeronPub<T> for dyn Stream<Burst<T>> {
    fn aeron_pub<B: AeronPublisherBackend + 'static>(
        self: &Rc<Self>,
        publisher: B,
        serialiser: impl Fn(&T) -> Vec<u8> + 'static,
    ) -> Rc<dyn Node> {
        build(self.clone(), serialiser, publisher)
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
}
