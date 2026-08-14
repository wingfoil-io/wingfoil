#![doc = include_str!("./README.md")]
//!
//! ```sh
//! cargo run --manifest-path crates/wingfoil/Cargo.toml --example top_of_book
//! cargo run --manifest-path crates/wingfoil/Cargo.toml --example top_of_book -- realtime
//! ```

use std::cell::RefCell;
use std::fmt;
use std::thread;
use std::time::Duration;

use wingfoil::channel::ChannelSender;
use wingfoil::prelude::*;
use wingfoil::{NanoTime, RunFor, RunMode};

/// How much of the file to replay, in seconds of market time. Bounded by time
/// rather than by message count because that is what bounds a *realtime* run:
/// this window takes exactly this long to arrive when the feed is live.
const SPAN_SECONDS: f64 = 3.0;

/// LOBSTER prices are in 1/10000 of a dollar.
const PRICE_SCALE: f64 = 10_000.0;

/// One row of the LOBSTER message file: a limit order, a cancel, or an
/// execution against a resting order.
#[derive(Debug, Clone, Default)]
struct Message {
    /// Nanoseconds since the *first* message in the file. Rebasing to zero
    /// keeps the printed clock readable; the real file starts at 09:30:00.
    offset: NanoTime,
    message_type: u8,
    order_id: u128,
    quantity: u64,
    price: u64,
    direction: i8,
}

/// The top of the book after a burst of messages has been applied. Either side
/// may be empty, so both are optional.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
struct Top {
    bid: Option<u64>,
    ask: Option<u64>,
}

/// A two-way quote, once both sides exist.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
struct Quote {
    bid: u64,
    ask: u64,
}

impl fmt::Display for Quote {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let bid = self.bid as f64 / PRICE_SCALE;
        let ask = self.ask as f64 / PRICE_SCALE;
        write!(
            f,
            "bid {bid:>7.2}  ask {ask:>7.2}  spread {:>5.2}  mid {:>7.2}",
            ask - bid,
            (bid + ask) / 2.0,
        )
    }
}

fn main() -> anyhow::Result<()> {
    let arg = std::env::args()
        .nth(1)
        .unwrap_or_else(|| "historical".into())
        .to_lowercase();
    let run_mode = match arg.as_str() {
        "realtime" => RunMode::RealTime,
        "historical" => RunMode::HistoricalFrom(NanoTime::ZERO),
        other => {
            eprintln!("unknown run mode: {other:?}. Use 'realtime' or 'historical'.");
            std::process::exit(1);
        }
    };

    let messages = load(SPAN_SECONDS)?;

    let g = GraphBuilder::new();
    // A `channel` source is the one source that works in *both* run modes: fed
    // with `send_at` it replays deterministically on the graph clock, fed with
    // `send` it delivers live. Nothing downstream of here knows which it got.
    let (inbound, sender) = g.channel::<Message>();

    // The book is construction-time state, owned by the `map` closure.
    let book = RefCell::new(lobster::OrderBook::default());

    // The apex. Both branches below read this one node, and the engine runs it
    // exactly once per cycle however many readers it has — the book is not
    // rebuilt per branch.
    let top = inbound.map(move |burst: &Burst<Message>| apply(burst, &book));

    // Each side of the book moves at its own rate: `distinct` reduces the
    // per-message stream to the cycles where that side actually changed.
    let bid = top.map(|t: &Top| t.bid).distinct();
    let ask = top.map(|t: &Top| t.ask).distinct();

    // The recombine. `join` fires when *either* side moves, which is exactly
    // when the quote changes.
    let quotes = bid
        .join(&ask, |b: &Option<u64>, a: &Option<u64>| match (b, a) {
            (Some(bid), Some(ask)) => Some(Quote {
                bid: *bid,
                ask: *ask,
            }),
            _ => None,
        })
        .filter_none()
        .distinct()
        // Engine time: replayed message time in a backtest, the wall clock
        // live. The values either side of it are identical in both.
        .with_time()
        .for_each(|(t, q): &(NanoTime, Quote)| {
            println!("{}  {q}", t.pretty());
            Ok(())
        });
    let n_quotes = quotes.count();

    // The producer: the only part of the program that knows about run mode.
    // Historical stamps each message with its own time and lets the engine
    // replay it; realtime hands them over at the pace they originally arrived.
    let feed = thread::spawn(move || produce(&sender, messages, run_mode));

    let mut runner = g.build();
    runner.run(run_mode, RunFor::Forever)?;
    feed.join().expect("feed thread panicked");

    println!("{} quote changes", runner.value(&n_quotes));
    Ok(())
}

/// Apply one burst of messages to the book and read its top.
///
/// The burst carries every message sharing a timestamp — one message or a
/// handful — so the book reaches a consistent state before its top is read.
fn apply(burst: &Burst<Message>, book: &RefCell<lobster::OrderBook>) -> Top {
    let mut bk = book.borrow_mut();
    for msg in burst.iter() {
        // Message type 5 is a hidden-order execution: no price information, so
        // it never reaches the book.
        if msg.message_type != 5 {
            bk.execute(order(msg));
        }
    }
    Top {
        bid: bk.max_bid(),
        ask: bk.min_ask(),
    }
}

/// Feed the graph. The wiring above is identical either way; only the pacing
/// and the timestamps differ, which is the whole point of the example.
fn produce(sender: &ChannelSender<Message>, messages: Vec<Message>, run_mode: RunMode) {
    let mut previous = NanoTime::ZERO;
    for msg in messages {
        match run_mode {
            // Deterministic replay: stamp each message with its own time and
            // let the engine schedule it. No clock is consulted.
            RunMode::HistoricalFrom(_) => {
                let at = msg.offset;
                sender.send_at(msg, at);
            }
            // Live: wait out the gap the message originally arrived after, then
            // hand it over. Engine time becomes the wall clock.
            RunMode::RealTime => {
                if msg.offset > previous {
                    thread::sleep(Duration::from(msg.offset - previous));
                }
                previous = msg.offset;
                sender.send(msg);
            }
        }
    }
    sender.close();
}

/// Translate a LOBSTER message into the book's order type.
fn order(msg: &Message) -> lobster::OrderType {
    let side = if msg.direction == 1 {
        lobster::Side::Bid
    } else {
        lobster::Side::Ask
    };
    match msg.message_type {
        // Submission of a new limit order.
        1 => lobster::OrderType::Limit {
            id: msg.order_id,
            side,
            price: msg.price,
            qty: msg.quantity,
        },
        // Cancellation (partial or full) of a resting order.
        2 | 3 => lobster::OrderType::Cancel { id: msg.order_id },
        // Execution against a resting order: the aggressor takes the other side.
        _ => lobster::OrderType::Market {
            id: msg.order_id,
            side: match side {
                lobster::Side::Bid => lobster::Side::Ask,
                lobster::Side::Ask => lobster::Side::Bid,
            },
            qty: msg.quantity,
        },
    }
}

/// Read the first `span` seconds of the LOBSTER sample, rebasing their
/// timestamps to the first message so the printed clock starts at zero.
fn load(span: f64) -> anyhow::Result<Vec<Message>> {
    let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("examples/core/order_book/data/aapl.csv");
    let text = std::fs::read_to_string(&path)
        .map_err(|e| anyhow::anyhow!("reading {}: {e}", path.display()))?;

    let mut base: Option<f64> = None;
    let mut out = Vec::new();
    for line in text.lines().skip(1) {
        let f: Vec<&str> = line.split(',').collect();
        if f.len() < 6 {
            anyhow::bail!("malformed LOBSTER row: {line:?}");
        }
        let seconds: f64 = f[0].parse()?;
        let base = *base.get_or_insert(seconds);
        if seconds - base > span {
            break;
        }
        out.push(Message {
            offset: NanoTime::from(((seconds - base) * 1e9) as u64),
            message_type: f[1].parse()?,
            order_id: f[2].parse()?,
            quantity: f[3].parse()?,
            price: f[4].parse()?,
            direction: f[5].parse()?,
        });
    }
    Ok(out)
}
