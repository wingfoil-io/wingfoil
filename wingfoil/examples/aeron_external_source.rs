//! External-source / inverted-control integration pattern.
//!
//! Demonstrates driving wingfoil from an Aeron poll loop that the caller
//! owns, using [`ExternalSource`] as the escape hatch between zero-copy
//! decode (inside the Aeron poll callback) and downstream graph processing.
//! This single example covers both halves of the pattern that previously
//! lived as two separate examples (`inverted_control_idle_strategy.rs` and
//! `inverted_control_publisher.rs`) — a synthetic publisher feeds bytes,
//! the subscriber decodes them inside its poll callback and pushes
//! extracted fields into source nodes, and a downstream wingfoil node
//! consumes those sources on every graph cycle driven by the same loop.
//!
//! # When to use this pattern
//!
//! Prefer the graph-native `aeron_sub_fragment` (returns a `Burst<T>`) for most
//! use cases. `ExternalSource` is the escape hatch when you need:
//! - Zero-copy SBE decoding with flyweight lifetimes (decoders only live
//!   during the poll callback).
//! - Integration with Aeron idle strategies (the caller's poll loop
//!   controls CPU management).
//! - Single-threaded HFT control flow where wingfoil's cycle yields to the
//!   external loop instead of owning it.
//!
//! # Pattern shape
//!
//! ```text
//! loop:
//!   1. publisher.offer(bytes)          // simulated source of inbound data
//!   2. subscriber.poll(|fragment| {    // zero-copy decode in callback
//!        let decoder = wrap(fragment);
//!        price_source.borrow_mut().set(decoder.price());
//!        qty_source.borrow_mut().set(decoder.quantity());
//!      })
//!   3. graph.run()                     // cycle reads from sources
//!   4. idle_strategy.idle(work_count)  // CPU management (sketch)
//! ```
//!
//! Run with: `cargo run --example aeron_external_source --features aeron`
//!
//! Note: requires the Aeron media driver to be running.

use std::cell::RefCell;
use std::rc::Rc;
use std::time::{Duration, Instant};
use wingfoil::adapters::aeron::rusteron_backend::{RusteronPublisher, RusteronSubscriber};
use wingfoil::adapters::aeron::{
    AeronHandle, AeronPublisherBackend, AeronSubscriberBackend, ExternalSource,
};
use wingfoil::{
    Graph, GraphState, IntoNode, MutableNode, RunFor, RunMode, StreamPeek, StreamPeekRef, UpStreams,
};

/// Simulated SBE-style zero-copy decoder. Only valid for the duration of
/// the poll callback (borrows from the fragment buffer).
struct OrderDecoder<'a> {
    buffer: &'a [u8],
}

impl<'a> OrderDecoder<'a> {
    fn wrap(buffer: &'a [u8], _offset: usize) -> Option<Self> {
        if buffer.len() >= 24 {
            Some(Self { buffer })
        } else {
            None
        }
    }

    fn price(&self) -> f64 {
        f64::from_le_bytes(self.buffer[0..8].try_into().unwrap())
    }

    fn quantity(&self) -> i64 {
        i64::from_le_bytes(self.buffer[8..16].try_into().unwrap())
    }

    fn side(&self) -> char {
        self.buffer[16] as char
    }
}

/// Downstream processor that reads from three `ExternalSource`s.
struct OrderProcessorNode<P, Q, S>
where
    P: StreamPeekRef<f64>,
    Q: StreamPeekRef<i64>,
    S: StreamPeekRef<char>,
{
    price_source: Rc<RefCell<P>>,
    qty_source: Rc<RefCell<Q>>,
    side_source: Rc<RefCell<S>>,
    total_volume: i64,
    message_count: usize,
}

impl<P, Q, S> OrderProcessorNode<P, Q, S>
where
    P: StreamPeekRef<f64>,
    Q: StreamPeekRef<i64>,
    S: StreamPeekRef<char>,
{
    fn new(
        price_source: Rc<RefCell<P>>,
        qty_source: Rc<RefCell<Q>>,
        side_source: Rc<RefCell<S>>,
    ) -> Self {
        Self {
            price_source,
            qty_source,
            side_source,
            total_volume: 0,
            message_count: 0,
        }
    }
}

impl<P, Q, S> MutableNode for OrderProcessorNode<P, Q, S>
where
    P: StreamPeekRef<f64> + 'static,
    Q: StreamPeekRef<i64> + 'static,
    S: StreamPeekRef<char> + 'static,
{
    fn cycle(&mut self, _state: &mut GraphState) -> anyhow::Result<bool> {
        let price = self.price_source.peek_value();
        let qty = self.qty_source.peek_value();
        let side = self.side_source.peek_value();

        if price == 0.0 {
            return Ok(false);
        }

        self.message_count += 1;
        self.total_volume += qty;

        println!(
            "  Processed: {} {} @ ${:.2} (total volume: {})",
            side, qty, price, self.total_volume
        );

        Ok(false)
    }

    fn start(&mut self, _state: &mut GraphState) -> anyhow::Result<()> {
        Ok(())
    }

    fn upstreams(&self) -> UpStreams {
        UpStreams::none()
    }
}

fn main() {
    println!("External-Source / Inverted-Control Pattern");
    println!("===========================================");
    println!();

    let handle = match AeronHandle::connect() {
        Ok(h) => h,
        Err(e) => {
            eprintln!("Failed to connect to Aeron media driver: {:?}", e);
            eprintln!("Make sure the Aeron media driver is running.");
            return;
        }
    };

    let channel = "aeron:ipc";
    let stream_id = 9003;
    let timeout = Duration::from_secs(5);

    // Publisher half — simulates an external source.
    let mut publisher: RusteronPublisher = match handle.publication(channel, stream_id, timeout) {
        Ok(p) => p,
        Err(e) => {
            eprintln!("Failed to create publication: {:?}", e);
            return;
        }
    };

    // Subscriber half — driven by the external poll loop below.
    let mut subscriber: RusteronSubscriber = match handle.subscription(channel, stream_id, timeout)
    {
        Ok(s) => s,
        Err(e) => {
            eprintln!("Failed to create subscription: {:?}", e);
            return;
        }
    };

    // Source nodes populated by the poll callback, consumed by the graph.
    let price_source = Rc::new(RefCell::new(ExternalSource::new(0.0_f64)));
    let qty_source = Rc::new(RefCell::new(ExternalSource::new(0_i64)));
    let side_source = Rc::new(RefCell::new(ExternalSource::new(' ')));

    let price_source_for_poll = price_source.clone();
    let qty_source_for_poll = qty_source.clone();
    let side_source_for_poll = side_source.clone();

    let processor = OrderProcessorNode::new(
        price_source.clone(),
        qty_source.clone(),
        side_source.clone(),
    );

    let mut graph = Graph::new(
        vec![
            price_source,
            qty_source,
            side_source,
            RefCell::new(processor).into_node(),
        ],
        RunMode::RealTime,
        RunFor::Cycles(1),
    );

    // Publisher-side feed: 5 simulated orders.
    let orders: Vec<(f64, i64, char)> = vec![
        (100.50, 1000, 'B'),
        (100.75, 500, 'S'),
        (100.25, 2000, 'B'),
        (101.00, 1500, 'S'),
        (100.50, 750, 'B'),
    ];

    let max_cycles = 50;
    let mut cycle_count = 0;
    let mut order_idx = 0;
    let start = Instant::now();

    println!(
        "Driving external loop: publish → poll(decode) → graph.run() (stream {})",
        stream_id
    );
    println!();

    loop {
        // 1. External producer (publisher half) feeds the wire.
        if order_idx < orders.len() {
            let (price, qty, side) = orders[order_idx];
            let mut buffer = Vec::with_capacity(24);
            buffer.extend_from_slice(&price.to_le_bytes());
            buffer.extend_from_slice(&qty.to_le_bytes());
            buffer.push(side as u8);
            buffer.resize(24, 0);
            match publisher.offer(&buffer) {
                Ok(_) => {
                    println!("Published: {} {} @ ${:.2}", side, qty, price);
                    order_idx += 1;
                }
                Err(_) => {
                    // back-pressure: try again next cycle
                }
            }
        }

        // 2. External poller (subscriber half) drives the zero-copy decode.
        let work_count = subscriber
            .poll(&mut |fragment: &[u8]| {
                if let Some(decoder) = OrderDecoder::wrap(fragment, 0) {
                    price_source_for_poll.borrow_mut().set(decoder.price());
                    qty_source_for_poll.borrow_mut().set(decoder.quantity());
                    side_source_for_poll.borrow_mut().set(decoder.side());
                }
            })
            .unwrap_or(0);

        // 3. wingfoil graph cycle reads the sources.
        if let Err(e) = graph.run() {
            eprintln!("Graph run error: {:?}", e);
            break;
        }

        // 4. Idle strategy sketch — in real HFT code, use
        //    `rusteron_client::BusySpinIdleStrategy::new().idle(work_count)`.
        if work_count == 0 {
            std::thread::sleep(Duration::from_millis(10));
        }

        cycle_count += 1;
        if cycle_count >= max_cycles && order_idx >= orders.len() {
            break;
        }
    }

    println!();
    println!(
        "Completed {} cycles in {:?} (processed {} orders)",
        cycle_count,
        start.elapsed(),
        order_idx
    );
}
