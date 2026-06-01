//! Circuit-breaker example driven by the Aeron status side-channel.
//!
//! Demonstrates [`aeron_sub_fragment_with_status`] from Story 12.5: the factory
//! returns `(data_stream, status_stream)`. This example wires a custom node
//! that consumes both streams. The status stream drives a "healthy" gate:
//! when the subscriber's last observed [`AeronStatus`] is `Connected`,
//! incoming data values are passed through; otherwise they are dropped (and
//! the drop is logged for visibility).
//!
//! Cross-references:
//! - [`aeron_sub_fragment_with_status`](wingfoil::adapters::aeron::aeron_sub_fragment_with_status)
//! - [`AeronStatusStream`](wingfoil::adapters::aeron::AeronStatusStream)
//!
//! Run with: `cargo run --example aeron_status_circuit_breaker --features aeron`

use std::cell::RefCell;
use std::rc::Rc;
use std::time::Duration;
use wingfoil::adapters::aeron::buffer::FragmentBuffer;
use wingfoil::adapters::aeron::error::TransportError;
use wingfoil::adapters::aeron::{
    AeronHandle, AeronPub, AeronStatus, AeronSubOptions, aeron_sub_fragment_with_status,
};
use wingfoil::*;

/// Custom node: data values pass through only while the latest status is
/// `Connected`. Anything else flips the breaker open.
struct CircuitBreakerNode {
    data: Rc<dyn Stream<Burst<i64>>>,
    status: Rc<dyn Stream<Burst<AeronStatus>>>,
    last_status: AeronStatus,
    passed: u64,
    dropped: u64,
}

impl CircuitBreakerNode {
    fn new(data: Rc<dyn Stream<Burst<i64>>>, status: Rc<dyn Stream<Burst<AeronStatus>>>) -> Self {
        Self {
            data,
            status,
            last_status: AeronStatus::default(),
            passed: 0,
            dropped: 0,
        }
    }
}

impl MutableNode for CircuitBreakerNode {
    fn cycle(&mut self, _state: &mut GraphState) -> anyhow::Result<bool> {
        // 1. Latch the latest status transition (if any).
        let status_burst = self.status.peek_value();
        if let Some(latest) = status_burst.last()
            && *latest != self.last_status
        {
            println!("breaker: status {:?} → {:?}", self.last_status, latest);
            self.last_status = latest.clone();
        }

        // 2. Gate the data burst on the healthy predicate.
        let healthy = matches!(self.last_status, AeronStatus::Connected);
        let data_burst = self.data.peek_value();
        for value in data_burst.iter() {
            if healthy {
                self.passed += 1;
                println!("breaker: ✓ value={value} (passed={})", self.passed);
            } else {
                self.dropped += 1;
                println!(
                    "breaker: ✗ value={value} dropped (status={:?}, dropped={})",
                    self.last_status, self.dropped
                );
            }
        }

        Ok(false)
    }

    fn upstreams(&self) -> UpStreams {
        UpStreams::new(
            vec![
                Dep::Active(self.data.clone()).as_node(),
                Dep::Active(self.status.clone()).as_node(),
            ],
            Vec::new(),
        )
    }
}

fn main() -> anyhow::Result<()> {
    env_logger::init();

    let handle = AeronHandle::connect()?;

    let channel = "aeron:ipc";
    // Stream IDs in the 9000+ range to avoid implied coordination with the
    // integration-test ranges (2014-2024).
    let stream_id = 9001i32;
    let timeout = Duration::from_secs(5);

    let sub = handle.subscription(channel, stream_id, timeout)?;
    let pub_ = handle.publication(channel, stream_id, timeout)?;

    // Typed parser: little-endian i64 fragments.
    let parser = |frag: &FragmentBuffer<'_>| -> Result<Option<i64>, TransportError> {
        Ok(frag.as_ref().try_into().ok().map(i64::from_le_bytes))
    };

    let (data_stream, status_stream) =
        aeron_sub_fragment_with_status::<i64, _, _>(sub, parser, AeronSubOptions::default());

    // Build a publisher that emits a small ramp of 10 values from a ticker.
    let publisher_node = wingfoil::ticker(Duration::from_millis(200))
        .count()
        .map(|n: u64| {
            let mut b: Burst<i64> = Burst::new();
            b.push(n as i64);
            b
        })
        .aeron_pub(pub_, |v: &i64| v.to_le_bytes().to_vec());

    let breaker = CircuitBreakerNode::new(data_stream, status_stream);
    let breaker_node: Rc<dyn Node> = RefCell::new(breaker).into_node();

    wingfoil::Graph::new(
        vec![breaker_node, publisher_node],
        RunMode::RealTime,
        RunFor::Duration(Duration::from_secs(3)),
    )
    .run()?;

    Ok(())
}
