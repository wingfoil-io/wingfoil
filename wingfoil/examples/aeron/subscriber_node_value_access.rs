//! Demonstrates value-based access with AeronSubscriberValueNode.
//!
//! This example shows:
//! - Creating an AeronSubscriberValueNode using the builder pattern
//! - Accessing values via `peek_value()` (value-based, cleaner syntax)
//! - Best for primitive types like i64, f64, bool
//!
//! Run with: `cargo run --example aeron_subscriber_node_value_access --features aeron`
//!
//! Note: Requires the Aeron media driver to be running.
//! See https://github.com/olibye/aerofoil for setup instructions.

use aerofoil::nodes::AeronSubscriberValueNode;
use aerofoil::transport::rusteron::RusteronSubscriber;
use rusteron_client::IntoCString;
use std::cell::RefCell;
use std::rc::Rc;
use std::time::Duration;
use wingfoil::{
    Graph, GraphState, IntoNode, MutableNode, RunFor, RunMode, StreamPeek, StreamPeekRef, UpStreams,
};

struct ValueAccessNode<T: StreamPeekRef<i64>> {
    upstream: Rc<RefCell<T>>,
    last_value: i64,
}

impl<T: StreamPeekRef<i64>> ValueAccessNode<T> {
    fn new(upstream: Rc<RefCell<T>>) -> Self {
        Self {
            upstream,
            last_value: 0,
        }
    }
}

impl<T: StreamPeekRef<i64> + 'static> MutableNode for ValueAccessNode<T> {
    fn cycle(&mut self, _state: &mut GraphState) -> anyhow::Result<bool> {
        let current = self.upstream.peek_value();

        if current != self.last_value {
            println!("Received new value via peek_value(): {}", current);
            self.last_value = current;
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
    println!("Value Access Pattern Example");
    println!("=============================");
    println!();
    println!("This example demonstrates AeronSubscriberValueNode with peek_value().");
    println!("Access pattern: self.upstream.peek_value() - cleaner than reference access!");
    println!();

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
    let stream_id = 3002;

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

    let subscriber_node = AeronSubscriberValueNode::builder()
        .subscriber(subscriber)
        .parser(parser)
        .default(0i64)
        .build();

    let downstream = ValueAccessNode::new(subscriber_node.clone());

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
