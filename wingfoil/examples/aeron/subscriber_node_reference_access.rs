//! Demonstrates reference-based access with AeronSubscriberValueRefNode.
//!
//! This example shows:
//! - Creating an AeronSubscriberValueRefNode using the builder pattern
//! - Accessing values via `peek_ref()` (reference-based)
//! - The dual-Rc pattern for graph integration
//!
//! Run with: `cargo run --example aeron_subscriber_node_reference_access --features aeron`
//!
//! Note: Requires the Aeron media driver to be running.
//! See https://github.com/olibye/aerofoil for setup instructions.

use aerofoil::nodes::AeronSubscriberValueRefNode;
use aerofoil::transport::rusteron::RusteronSubscriber;
use rusteron_client::IntoCString;
use std::cell::RefCell;
use std::time::Duration;
use wingfoil::{
    Graph, GraphState, IntoNode, MutableNode, RunFor, RunMode, StreamPeekRef, UpStreams,
};

/// Example downstream node that uses peek_ref() for reference access
struct ReferenceAccessNode<T: StreamPeekRef<i64>> {
    upstream: std::rc::Rc<RefCell<T>>,
    last_value: i64,
}

impl<T: StreamPeekRef<i64>> ReferenceAccessNode<T> {
    fn new(upstream: std::rc::Rc<RefCell<T>>) -> Self {
        Self {
            upstream,
            last_value: 0,
        }
    }
}

impl<T: StreamPeekRef<i64> + 'static> MutableNode for ReferenceAccessNode<T> {
    fn cycle(&mut self, _state: &mut GraphState) -> anyhow::Result<bool> {
        // Reference access pattern: borrow -> peek_ref -> deref
        let current = *self.upstream.borrow().peek_ref();

        if current != self.last_value {
            println!("Received new value via peek_ref(): {}", current);
            self.last_value = current;
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

fn main() {
    println!("Reference Access Pattern Example");
    println!("=================================");
    println!();
    println!("This example demonstrates AeronSubscriberValueRefNode with peek_ref().");
    println!("Access pattern: *self.upstream.borrow().peek_ref()");
    println!();

    // Create Aeron context and connection
    let context = match rusteron_client::AeronContext::new() {
        Ok(ctx) => ctx,
        Err(e) => {
            eprintln!("Failed to create Aeron context: {:?}", e);
            eprintln!("Make sure the Aeron media driver is running.");
            eprintln!("See https://github.com/olibye/aerofoil for setup instructions.");
            return;
        }
    };

    let aeron = rusteron_client::Aeron::new(&context).expect("Failed to create Aeron");
    aeron.start().expect("Failed to start Aeron");

    let channel = "aeron:ipc";
    let stream_id = 3001;

    // Create subscriber
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

    // Parser for i64 values
    let parser = |fragment: &[u8]| -> Option<i64> {
        if fragment.len() >= 8 {
            Some(i64::from_le_bytes(fragment[0..8].try_into().ok()?))
        } else {
            None
        }
    };

    // Build subscriber node using builder pattern
    let subscriber_node = AeronSubscriberValueRefNode::builder()
        .subscriber(subscriber)
        .parser(parser)
        .default(0i64)
        .build_ref();

    // Create downstream node with reference access
    let downstream = ReferenceAccessNode::new(subscriber_node.clone());

    // Build and run graph
    let mut graph = Graph::new(
        vec![subscriber_node, RefCell::new(downstream).into_node()],
        RunMode::RealTime,
        RunFor::Cycles(10),
    );

    println!("Running graph for 10 cycles on stream {}...", stream_id);
    println!(
        "(Publish i64 values to aeron:ipc stream {} to see them)",
        stream_id
    );
    println!();

    graph.run().expect("Graph execution failed");

    println!("Graph completed.");
}
