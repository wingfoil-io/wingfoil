//! Demonstrates the dual-Rc pattern for Wingfoil graph integration.
//!
//! This example shows:
//! - Why the dual-Rc pattern is needed for graph integration
//! - How to share a node between the graph and downstream nodes
//! - Using the builder pattern which handles this automatically
//!
//! Run with: `cargo run --example aeron_dual_rc_pattern --features aeron`
//!
//! Note: Requires the Aeron media driver to be running.
//! See https://github.com/olibye/aerofoil for setup instructions.

use aerofoil::nodes::AeronSubscriberValueRefNode;
use aerofoil::transport::rusteron::RusteronSubscriber;
use rusteron_client::IntoCString;
use std::cell::RefCell;
use std::rc::Rc;
use std::time::Duration;
use wingfoil::{
    Graph, GraphState, IntoNode, MutableNode, RunFor, RunMode, StreamPeekRef, UpStreams,
};

/// Example downstream node demonstrating the dual-Rc pattern
struct DownstreamNode<T: StreamPeekRef<i64>> {
    upstream: Rc<RefCell<T>>,
    sum: i64,
}

impl<T: StreamPeekRef<i64>> DownstreamNode<T> {
    fn new(upstream: Rc<RefCell<T>>) -> Self {
        Self { upstream, sum: 0 }
    }
}

impl<T: StreamPeekRef<i64> + 'static> MutableNode for DownstreamNode<T> {
    fn cycle(&mut self, _state: &mut GraphState) -> anyhow::Result<bool> {
        let value = *self.upstream.borrow().peek_ref();
        self.sum += value;
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
    println!("Dual-Rc Pattern Example");
    println!("=======================");
    println!();
    println!("The dual-Rc pattern is needed because:");
    println!("  - Downstream nodes need the concrete type to call peek_ref()");
    println!("  - The graph needs Rc<dyn Node> for its heterogeneous vector");
    println!("  - Wingfoil's into_node() consumes the value");
    println!();
    println!("Solution: Use the builder pattern which handles this automatically!");
    println!();

    // Create Aeron context and connection
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
    let stream_id = 3003;

    let async_sub = aeron
        .async_add_subscription(
            &channel.into_c_string(),
            stream_id,
            rusteron_client::Handlers::no_available_image_handler(),
            rusteron_client::Handlers::no_unavailable_image_handler(),
        )
        .expect("Failed to start subscription");

    let subscription = async_sub
        .poll_blocking(Duration::from_secs(5))
        .expect("Failed to complete subscription");

    let subscriber = RusteronSubscriber::new(subscription);

    let parser = |fragment: &[u8]| -> Option<i64> {
        if fragment.len() >= 8 {
            Some(i64::from_le_bytes(fragment[0..8].try_into().ok()?))
        } else {
            None
        }
    };

    // === THE BUILDER PATTERN (RECOMMENDED) ===
    // The builder returns Rc<RefCell<...>> which can be:
    // - Cloned for the graph (coerces to Rc<dyn Node>)
    // - Used directly by downstream nodes
    let subscriber_node = AeronSubscriberValueRefNode::builder()
        .subscriber(subscriber)
        .parser(parser)
        .default(0i64)
        .build_ref();

    // Clone for downstream (keeps concrete type)
    let upstream_ref = subscriber_node.clone();

    // Create downstream node with the concrete type
    let downstream = DownstreamNode::new(upstream_ref);

    // Build graph - subscriber_node coerces to Rc<dyn Node>
    let mut graph = Graph::new(
        vec![
            subscriber_node,                      // Coerces to Rc<dyn Node>
            RefCell::new(downstream).into_node(), // Uses into_node()
        ],
        RunMode::RealTime,
        RunFor::Cycles(5),
    );

    println!("Running graph for 5 cycles...");
    graph.run().expect("Graph execution failed");
    println!("Graph completed.");

    // === THE MANUAL PATTERN (FOR REFERENCE) ===
    // This is what happens under the hood:
    //
    // let node = AeronSubscriberValueRefNode::new(subscriber, parser, 0i64);
    // let node_rc: Rc<RefCell<_>> = Rc::new(RefCell::new(node));
    // let upstream_ref = node_rc.clone();           // For downstream
    // let graph_node: Rc<dyn Node> = node_rc;       // For graph
}
