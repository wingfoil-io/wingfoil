//! Demonstrates the fan-out pattern: one input feeding multiple outputs.
//!
//! This example shows:
//! - Single AeronSubscriberValueNode shared by multiple downstream nodes
//! - Publisher-in-callback pattern for Aeron output
//! - Complete Aeron-to-Aeron data flow
//!
//! Run with: `cargo run --example aeron_fan_out_pattern --features aeron`
//!
//! Note: Requires the Aeron media driver to be running.
//! See https://github.com/olibye/aerofoil for setup instructions.

use aerofoil::nodes::AeronSubscriberValueNode;
use aerofoil::transport::AeronPublisher;
use aerofoil::transport::rusteron::{RusteronPublisher, RusteronSubscriber};
use rusteron_client::IntoCString;
use std::cell::RefCell;
use std::rc::Rc;
use std::time::Duration;
use wingfoil::{
    Graph, GraphState, IntoNode, MutableNode, RunFor, RunMode, StreamPeekRef, UpStreams,
};

/// Node that sums values and publishes to Aeron
struct SummingPublisher<T: StreamPeekRef<i64>, P: AeronPublisher> {
    upstream: Rc<RefCell<T>>,
    publisher: Rc<RefCell<P>>,
    sum: i64,
    last_value: i64,
}

impl<T: StreamPeekRef<i64>, P: AeronPublisher> SummingPublisher<T, P> {
    fn new(upstream: Rc<RefCell<T>>, publisher: Rc<RefCell<P>>) -> Self {
        Self {
            upstream,
            publisher,
            sum: 0,
            last_value: 0,
        }
    }
}

impl<T: StreamPeekRef<i64> + 'static, P: AeronPublisher + 'static> MutableNode
    for SummingPublisher<T, P>
{
    fn cycle(&mut self, _state: &mut GraphState) -> anyhow::Result<bool> {
        let current = *self.upstream.borrow().peek_ref();

        if current != self.last_value {
            self.sum += current;
            self.last_value = current;

            // Publish sum to Aeron
            let _ = self.publisher.borrow_mut().offer(&self.sum.to_le_bytes());
            println!("Published sum: {}", self.sum);
        }

        Ok(false)
    }

    fn start(&mut self, state: &mut GraphState) -> anyhow::Result<()> {
        state.always_callback();
        Ok(())
    }

    fn upstreams(&self) -> UpStreams {
        UpStreams::none()
    }
}

/// Node that counts values and publishes to Aeron
struct CountingPublisher<T: StreamPeekRef<i64>, P: AeronPublisher> {
    upstream: Rc<RefCell<T>>,
    publisher: Rc<RefCell<P>>,
    count: i64,
    last_value: i64,
}

impl<T: StreamPeekRef<i64>, P: AeronPublisher> CountingPublisher<T, P> {
    fn new(upstream: Rc<RefCell<T>>, publisher: Rc<RefCell<P>>) -> Self {
        Self {
            upstream,
            publisher,
            count: 0,
            last_value: 0,
        }
    }
}

impl<T: StreamPeekRef<i64> + 'static, P: AeronPublisher + 'static> MutableNode
    for CountingPublisher<T, P>
{
    fn cycle(&mut self, _state: &mut GraphState) -> anyhow::Result<bool> {
        let current = *self.upstream.borrow().peek_ref();

        if current != self.last_value || self.count == 0 {
            self.count += 1;
            self.last_value = current;

            // Publish count to Aeron
            let _ = self.publisher.borrow_mut().offer(&self.count.to_le_bytes());
            println!("Published count: {}", self.count);
        }

        Ok(false)
    }

    fn start(&mut self, state: &mut GraphState) -> anyhow::Result<()> {
        state.always_callback();
        Ok(())
    }

    fn upstreams(&self) -> wingfoil::UpStreams {
        UpStreams::none()
    }
}

fn main() {
    println!("Fan-Out Pattern Example");
    println!("=======================");
    println!();
    println!("Architecture:");
    println!("  Stream 3004 (input) --> Subscriber --> SummingPublisher --> Stream 3005 (sum)");
    println!("                              |");
    println!("                              +------> CountingPublisher --> Stream 3006 (count)");
    println!();

    // Create Aeron context
    let context = match rusteron_client::AeronContext::new() {
        Ok(ctx) => ctx,
        Err(e) => {
            eprintln!("Failed to create Aeron context: {:?}", e);
            eprintln!("Make sure the Aeron media driver is running.");
            return;
        }
    };

    let aeron = rusteron_client::Aeron::new(&context).expect("Failed to create Aeron");
    aeron.start().expect("Failed to start Aeron");

    let channel = "aeron:ipc";
    let input_stream = 3004;
    let sum_stream = 3005;
    let count_stream = 3006;

    // Create input subscriber
    let input_sub = aeron
        .async_add_subscription(
            &channel.into_c_string(),
            input_stream,
            rusteron_client::Handlers::no_available_image_handler(),
            rusteron_client::Handlers::no_unavailable_image_handler(),
        )
        .expect("Failed to start subscription")
        .poll_blocking(Duration::from_secs(5))
        .expect("Failed to complete subscription");
    let input_subscriber = RusteronSubscriber::new(input_sub);

    // Create output publishers
    let sum_pub = aeron
        .async_add_publication(&channel.into_c_string(), sum_stream)
        .expect("Failed to start sum publication")
        .poll_blocking(Duration::from_secs(5))
        .expect("Failed to complete sum publication");
    let sum_publisher = Rc::new(RefCell::new(RusteronPublisher::new(sum_pub)));

    let count_pub = aeron
        .async_add_publication(&channel.into_c_string(), count_stream)
        .expect("Failed to start count publication")
        .poll_blocking(Duration::from_secs(5))
        .expect("Failed to complete count publication");
    let count_publisher = Rc::new(RefCell::new(RusteronPublisher::new(count_pub)));

    // Parser for i64 values
    let parser = |fragment: &[u8]| -> Option<i64> {
        if fragment.len() >= 8 {
            Some(i64::from_le_bytes(fragment[0..8].try_into().ok()?))
        } else {
            None
        }
    };

    // Build input subscriber node (shared by both downstream nodes)
    let subscriber_node = AeronSubscriberValueNode::builder()
        .subscriber(input_subscriber)
        .parser(parser)
        .default(0i64)
        .build();

    // Create downstream nodes (fan-out: both share same upstream)
    let summing = SummingPublisher::new(subscriber_node.clone(), sum_publisher);
    let counting = CountingPublisher::new(subscriber_node.clone(), count_publisher);

    // Build graph
    let mut graph = Graph::new(
        vec![
            subscriber_node,
            RefCell::new(summing).into_node(),
            RefCell::new(counting).into_node(),
        ],
        RunMode::RealTime,
        RunFor::Cycles(10),
    );

    println!("Running graph for 10 cycles...");
    println!(
        "(Publish i64 values to aeron:ipc stream {} to see fan-out)",
        input_stream
    );
    println!();

    graph.run().expect("Graph execution failed");

    println!();
    println!("Graph completed.");
    println!("Sum was published to stream {}", sum_stream);
    println!("Count was published to stream {}", count_stream);
}
