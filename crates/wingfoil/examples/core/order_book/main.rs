#![doc = include_str!("./README.md")]
//!
//! ```sh
//! cargo run -p wingfoil --features csv --example order_book
//! ```

use std::cell::RefCell;
use std::path::PathBuf;
use std::time::Instant;

use serde::{Deserialize, Serialize};

use wingfoil::adapters::csv::{CsvSinkOps, csv_read};
use wingfoil::prelude::*;
use wingfoil::{NanoTime, RunFor, RunMode};

/// One row of the LOBSTER message file: a limit order, a cancel, or an
/// execution against a resting order. serde field names are the CSV columns.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
struct Message {
    /// Seconds since midnight — the event time driving the graph clock.
    seconds: f64,
    message_type: u8,
    order_id: u128,
    quantity: u64,
    price: u64,
    direction: i8,
}

/// Top of book after applying a burst of messages. Either side may be empty.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
struct TwoWayPrice {
    bid_price: Option<u64>,
    ask_price: Option<u64>,
}

/// One execution, from the aggressor's point of view.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
enum FillSide {
    #[default]
    HitBid,
    LiftAsk,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
struct Fill {
    side: FillSide,
    price: u64,
    quantity: u64,
}

fn main() -> anyhow::Result<()> {
    let data = data_dir();
    let source_path = data.join("aapl.csv");
    let fills_path = data.join("fills.csv");
    let prices_path = data.join("prices.csv");

    // The book is construction-time state, owned by the `map` closure.
    let book = RefCell::new(lobster::OrderBook::default());
    // Seconds since midnight -> graph time, so replay honours the data's own
    // schedule rather than the wall clock.
    let get_time = |msg: &Message| NanoTime::new((msg.seconds * 1e9) as u64);

    let g = GraphBuilder::new();
    // One node maintains the book and emits both outputs as a pair; `split`
    // decomposes that into two streams, each with its own frequency.
    let (fills, prices) = csv_read(&g, &source_path, get_time, true, None)?
        .map(move |chunk: &Burst<Message>| process_orders(chunk, &book))
        .split();

    let _prices_sink = prices.filter_none().distinct().csv_write(&prices_path)?;
    let _fills_sink = fills.csv_write(&fills_path)?;

    let mut runner = g.build();
    let started = Instant::now();
    runner.run(RunMode::HistoricalFrom(NanoTime::ZERO), RunFor::Forever)?;
    let elapsed = started.elapsed();

    println!("replayed {}", rel(&source_path));
    println!(
        "  {} two-way prices -> {}",
        count_rows(&prices_path)?,
        rel(&prices_path)
    );
    println!(
        "  {} fills          -> {}",
        count_rows(&fills_path)?,
        rel(&fills_path)
    );
    println!("in {elapsed:.3?}");
    Ok(())
}

/// Apply one burst of messages to the book, returning the fills they generated
/// and the resulting top of book.
///
/// The burst carries every message sharing a timestamp — one message or a
/// handful — so the book is advanced to a consistent state before its top is
/// read.
fn process_orders(
    chunk: &Burst<Message>,
    book: &RefCell<lobster::OrderBook>,
) -> (Burst<Fill>, Option<TwoWayPrice>) {
    let mut fills: Burst<Fill> = Burst::new();
    let mut price: Option<TwoWayPrice> = None;
    for msg in chunk.iter() {
        // Message type 5 is a hidden-order execution: no price information, so
        // it never reaches the book.
        if msg.message_type != 5 {
            let order = parse_order(msg);
            let mut bk = book.borrow_mut();
            let event = bk.execute(order);
            for fill in parse_fills(event) {
                fills.push(fill);
            }
            price = Some(TwoWayPrice {
                bid_price: bk.max_bid(),
                ask_price: bk.min_ask(),
            });
        }
    }
    (fills, price)
}

trait AsFill {
    fn as_fill(&self) -> Fill;
}

impl AsFill for lobster::FillMetadata {
    fn as_fill(&self) -> Fill {
        Fill {
            side: match self.taker_side {
                lobster::Side::Ask => FillSide::HitBid,
                lobster::Side::Bid => FillSide::LiftAsk,
            },
            price: self.price,
            quantity: self.qty,
        }
    }
}

fn parse_fills(event: lobster::OrderEvent) -> Vec<Fill> {
    match event {
        lobster::OrderEvent::Filled { fills, .. }
        | lobster::OrderEvent::PartiallyFilled { fills, .. } => {
            fills.into_iter().map(|meta| meta.as_fill()).collect()
        }
        _ => vec![],
    }
}

fn parse_order(msg: &Message) -> lobster::OrderType {
    let id = msg.order_id;
    let qty = msg.quantity;
    let price = msg.price;
    let side = if msg.direction == 1 {
        lobster::Side::Bid
    } else if msg.direction == -1 {
        lobster::Side::Ask
    } else {
        panic!("failed to parse direction: {}", msg.direction)
    };
    match msg.message_type {
        // New limit order.
        1 => lobster::OrderType::Limit {
            id,
            side,
            qty,
            price,
        },
        // 2 is a partial cancel, 3 a complete one. TODO: model 2 accurately.
        2 | 3 => lobster::OrderType::Cancel { id },
        // Execution against a resting order: replay it as the aggressor.
        4 => lobster::OrderType::Limit {
            id,
            side: !side,
            qty,
            price,
        },
        other => panic!("unrecognised message type: {other}"),
    }
}

/// The example's own `data/` directory, resolved from the crate root at compile
/// time so the example runs from anywhere.
fn data_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("examples/core/order_book/data")
}

/// Print paths relative to the crate root, so the output does not depend on
/// where the repository happens to be checked out.
fn rel(path: &std::path::Path) -> String {
    path.strip_prefix(env!("CARGO_MANIFEST_DIR"))
        .unwrap_or(path)
        .display()
        .to_string()
}

fn count_rows(path: &std::path::Path) -> anyhow::Result<usize> {
    // Every file written by `csv_write` carries a header row.
    Ok(std::fs::read_to_string(path)?
        .lines()
        .count()
        .saturating_sub(1))
}
