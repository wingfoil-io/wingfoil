#![doc = include_str!("./README.md")]

use wingfoil::adapters::csv_streams::*;
use wingfoil::{Graph, NanoTime, RunFor, RunMode, StreamOperators, TupleStreamOperators};

use serde::{Deserialize, Serialize};
use std::cell::RefCell;
use std::env;

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
struct Message {
    seconds: f64,
    message_type: u8,
    order_id: u128,
    quantity: u64,
    price: u64,
    direction: i8,
}

#[doc(hidden)]
pub fn main() {
    env_logger::init();
    set_current_dir();
    let book = RefCell::new(lobster::OrderBook::default());
    // map from seconds from midnight to NanoTime time
    let get_time = |msg: &Message| NanoTime::new((msg.seconds * 1e9) as u64);
    let (fills, prices) = csv_read_vec("aapl.csv", get_time, true)
        .map(move |chunk| process_orders(chunk, &book))
        .split();
    let prices_export = prices
        .filter_value(|price| !price.is_none())
        .map(|price| price.unwrap())
        .distinct()
        .csv_write("prices.csv");
    let fills_export = fills.csv_write_vec("fills.csv");
    let run_mode = RunMode::HistoricalFrom(NanoTime::ZERO);
    let run_for = RunFor::Forever;
    Graph::new(vec![prices_export, fills_export], run_mode, run_for)
        .print()
        .run()
        .unwrap();
}

fn set_current_dir() {
    let mut root = env::current_exe()
        .unwrap()
        .ancestors()
        .nth(4)
        .unwrap()
        .to_path_buf();
    root.push("wingfoil/examples/order_book/data/");
    env::set_current_dir(&root).unwrap();
}

fn process_orders(chunk: Vec<Message>, book: &RefCell<lobster::OrderBook>) -> (Vec<Fill>, Option<TwoWayPrice>) {
    let mut fills = vec![];
    let mut price: Option<TwoWayPrice> = None;
    // chunk contains messages with common timestamp.
    // could be 1 message or a handful of messages
    for msg in chunk {
        // message type 5 is hidden order execution
        // skip these as they do not contain any
        // price information
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

#[derive(Debug, Clone, Default, Eq, PartialEq, Serialize, Deserialize)]
struct TwoWayPrice {
    bid_price: Option<u64>,
    ask_price: Option<u64>,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
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
        lobster::OrderEvent::Filled {
            id: _,
            filled_qty: _,
            fills,
        } => fills
            .into_iter()
            .map(|fill_meta| fill_meta.as_fill())
            .collect(),
        lobster::OrderEvent::PartiallyFilled {
            id: _,
            filled_qty: _,
            fills,
        } => fills
            .into_iter()
            .map(|fill_meta| fill_meta.as_fill())
            .collect(),
        _ => vec![],
    }
}

fn parse_order(msg: Message) -> lobster::OrderType {
    let message_type = msg.message_type;
    let id = msg.order_id;
    let qty = msg.quantity;
    let price = msg.price;
    let side = if msg.direction == 1 {
        lobster::Side::Bid
    } else if msg.direction == -1 {
        lobster::Side::Ask
    } else {
        panic!("Failed to parse direction")
    };
    if message_type == 1 {
        // order
        lobster::OrderType::Limit { id, side, qty, price }
    } else if message_type == 2 || message_type == 3 {
        // 2 partial cancel.  TODO: implement more accurately
        // 3 complete cancel
        lobster::OrderType::Cancel { id }
    } else if message_type == 4 {
        // fill
        lobster::OrderType::Limit {
            id,
            side: !side,
            qty,
            price,
        }
    } else {
        panic!("unrecognised order type: {:}", message_type);
    }
}
