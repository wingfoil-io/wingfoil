#![doc = include_str!("./README.md")]
//!
//! ```sh
//! cargo run --manifest-path crates/wingfoil/Cargo.toml --example top_of_book
//! cargo run --manifest-path crates/wingfoil/Cargo.toml --example top_of_book -- realtime
//! ```

use std::cell::RefCell;
use std::fmt;
use std::thread;
use std::time::{Duration, Instant};

use wingfoil::fluent::Stream;
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

// ── the market-data seam ────────────────────────────────────────────────────

/// A connected feed: the inbound message stream, plus the producer driving it.
struct Feed {
    messages: Stream<Burst<Message>>,
    producer: thread::JoinHandle<()>,
}

/// Where market data comes from.
///
/// This is the seam a deployment actually swaps: one implementation replays a
/// file on the graph clock, the other delivers it at wall-clock pace, and a
/// third would hold a socket. **Everything downstream of `connect` is wired
/// once and cannot tell which it got** — which is the reason there is no second
/// implementation of the strategy to drift.
trait MarketData {
    fn connect(&self, g: &GraphBuilder) -> anyhow::Result<Feed>;
}

/// Deterministic replay. Every message is stamped with its own time and
/// scheduled by the engine, which consults no clock at all.
struct Replay {
    messages: Vec<Message>,
}

/// A live feed. Messages are handed over at the pace they originally arrived,
/// and engine time becomes the wall clock.
///
/// A file replayed at its original pace is a stand-in for a socket, not a
/// socket. What is *not* a stand-in is the seam: a real feed implements this
/// same trait and nothing below it changes.
struct LiveFeed {
    messages: Vec<Message>,
}

impl MarketData for Replay {
    fn connect(&self, g: &GraphBuilder) -> anyhow::Result<Feed> {
        let (messages, sender) = g.channel::<Message>();
        let batch = self.messages.clone();
        Ok(Feed {
            messages,
            producer: thread::spawn(move || {
                for msg in batch {
                    let at = msg.offset;
                    sender.send_at(msg, at);
                }
                sender.close();
            }),
        })
    }
}

impl MarketData for LiveFeed {
    fn connect(&self, g: &GraphBuilder) -> anyhow::Result<Feed> {
        let (messages, sender) = g.channel::<Message>();
        let batch = self.messages.clone();
        Ok(Feed {
            messages,
            producer: thread::spawn(move || {
                let mut previous = NanoTime::ZERO;
                for msg in batch {
                    if msg.offset > previous {
                        thread::sleep(Duration::from(msg.offset - previous));
                    }
                    previous = msg.offset;
                    sender.send(msg);
                }
                sender.close();
            }),
        })
    }
}

/// Pick the feed for this run mode — the only place the program branches on it.
fn market_data(run_mode: RunMode) -> anyhow::Result<Box<dyn MarketData>> {
    let messages = load(SPAN_SECONDS)?;
    Ok(match run_mode {
        RunMode::RealTime => Box::new(LiveFeed { messages }),
        RunMode::HistoricalFrom(_) => Box::new(Replay { messages }),
    })
}

// ── the graph ───────────────────────────────────────────────────────────────

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

    let g = GraphBuilder::new();
    // The one line that differs between a backtest and production.
    let feed = market_data(run_mode)?.connect(&g)?;

    // The book is construction-time state, owned by the `map` closure.
    let book = RefCell::new(lobster::OrderBook::default());

    // The apex. Both branches below read this one node, and the engine runs it
    // exactly once per cycle however many readers it has — the book is not
    // rebuilt per branch.
    let top = feed
        .messages
        .map(move |burst: &Burst<Message>| apply(burst, &book));

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

    let mut runner = g.build();
    let started = Instant::now();
    runner.run(run_mode, RunFor::Forever)?;
    let elapsed = started.elapsed();
    feed.producer.join().expect("feed thread panicked");

    // A backtest's headline number: how much market time went through, and how
    // long the wall clock took to do it. Meaningless in realtime, where the two
    // are the same thing by construction.
    let market = Duration::from_secs_f64(SPAN_SECONDS);
    println!("{} quote changes", runner.value(&n_quotes));
    match run_mode {
        RunMode::HistoricalFrom(_) => println!(
            "{market:.1?} of market data replayed in {elapsed:.3?} — {:.0}× faster than real time",
            market.as_secs_f64() / elapsed.as_secs_f64(),
        ),
        RunMode::RealTime => println!("{elapsed:.3?} elapsed, paced by the feed"),
    }
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
