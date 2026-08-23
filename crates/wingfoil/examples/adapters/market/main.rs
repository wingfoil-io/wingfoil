#![doc = include_str!("./README.md")]
//!
//! ```sh
//! cargo run -p wingfoil --features market,fix,kdb --example market_adapter
//! ```

use std::fmt;
use std::sync::Arc;
use std::time::{Duration, Instant};

use anyhow::{Context, Result, bail};

use wingfoil::adapters::fix::{FixMessage, fix_connect_tls};
use wingfoil::adapters::kdb::{
    KdbConnection, KdbDeserialize, KdbError, Row, SymbolInterner, kdb_read,
};
use wingfoil::adapters::market::{
    BookSnapshot, BookUpdate, InstrumentId, Level, MarketBookOps, OrderBook, Px, Qty, SCALE,
    Sequencing, Side,
};
use wingfoil::async_source::RunParams;
use wingfoil::prelude::*;
use wingfoil::{NanoTime, RunFor, RunMode};

// ── The swap point ───────────────────────────────────────────────────────────

/// One trait, one implementation per run mode.
///
/// `wire` builds a feed's source subgraph and normalises it into the
/// venue-neutral market vocabulary; `run_mode` names the clock that wiring
/// needs. Everything downstream of the returned stream is shared, so the
/// strategy in [`main`] is written once and cannot drift between backtest and
/// live — the property the vocabulary exists to buy.
trait FeedBuilder {
    /// The run mode this feed drives the graph under.
    fn run_mode(&self) -> RunMode;

    /// How long the run lasts.
    fn run_for(&self) -> RunFor;

    /// Wire the feed onto `g`, normalised to a stream of book updates.
    fn wire(&self, g: &GraphBuilder) -> Result<Stream<Burst<BookUpdate>>>;
}

fn main() -> Result<()> {
    env_logger::init();
    let feed = feed_from_args()?;

    let g = GraphBuilder::new();
    let updates = feed.wire(&g)?;
    // Count updates, not ticks: a burst can carry several (same-instant rows
    // under replay) or none (a live tick that decoded to no snapshot).
    let n_updates = updates.fold(0u64, |n, burst| *n += burst.len() as u64);

    // ── The strategy — written once, against the vocabulary ─────────────────
    // Maintain a book, watch its top, print only when the top moves. Neither
    // line knows (or can find out) which venue is upstream.
    let books = updates.order_book();
    let _tops = books
        .map(|book: &Arc<OrderBook>| TopOfBook::of(book))
        .distinct()
        .with_time()
        .for_each(|(t, top)| {
            println!("{}  {top}", clock(*t));
            Ok(())
        });

    let mut runner = g.build();
    let started = Instant::now();
    runner.run(feed.run_mode(), feed.run_for())?;
    let elapsed = started.elapsed();

    println!("{} book updates in {elapsed:.3?}", runner.value(&n_updates));
    Ok(())
}

const USAGE: &str = "usage: market_adapter [--historical | --live]

  --historical   replay recorded quotes from a kdb+ `quotes` table (default);
                 KDB_HOST / KDB_PORT override localhost:5000
  --live         subscribe to the LMAX London demo over FIX/TLS;
                 requires LMAX_USERNAME and LMAX_PASSWORD";

fn feed_from_args() -> Result<Box<dyn FeedBuilder>> {
    let mut args = std::env::args().skip(1);
    let mode = args.next();
    if let Some(extra) = args.next() {
        bail!("unexpected extra argument {extra:?}\n\n{USAGE}");
    }
    match mode.as_deref() {
        None | Some("--historical") => {
            let host = std::env::var("KDB_HOST").unwrap_or_else(|_| "localhost".into());
            let port = std::env::var("KDB_PORT")
                .ok()
                .map(|p| p.parse())
                .transpose()
                .context("KDB_PORT must be a port number")?
                .unwrap_or(5000);
            Ok(Box::new(KdbFeed {
                connection: KdbConnection::new(host, port),
                start: NanoTime::from_kdb_timestamp(0),
                window: Duration::from_secs(70),
            }))
        }
        Some("--live") => Ok(Box::new(LmaxFeed {
            username: std::env::var("LMAX_USERNAME").context(
                "LMAX_USERNAME not set — register a free demo account at \
                 https://register.london-demo.lmax.com/registration/LMB/",
            )?,
            password: std::env::var("LMAX_PASSWORD").context("LMAX_PASSWORD not set")?,
            window: Duration::from_secs(60),
        })),
        Some(other) => bail!("unrecognised argument {other:?}\n\n{USAGE}"),
    }
}

// ── Historical: kdb+ replay ──────────────────────────────────────────────────

/// EUR/USD top-of-book quotes recorded in a kdb+ table, replayed
/// deterministically at their own timestamps under
/// [`RunMode::HistoricalFrom`].
struct KdbFeed {
    connection: KdbConnection,
    start: NanoTime,
    window: Duration,
}

/// Prices in the store are integer **pipettes** (10⁻⁵ of the quote currency)
/// and sizes whole units of the base — exact integers end to end, so the
/// replayed book carries the recorded prices bit-for-bit. `SCALE` is the
/// fixed-point scale of [`Px`]/[`Qty`] (10⁹ per unit).
const PIPETTE: i128 = SCALE / 100_000;

/// One recorded quote row: `time, bid, bidQty, ask, askQty`.
#[derive(Debug, Clone, Default)]
struct QuoteRow {
    bid: i64,
    bid_qty: i64,
    ask: i64,
    ask_qty: i64,
}

impl KdbDeserialize for QuoteRow {
    fn from_kdb_row(
        row: Row<'_>,
        _columns: &[String],
        _interner: &mut SymbolInterner,
    ) -> Result<(NanoTime, Self), KdbError> {
        let time = row.get_timestamp(0)?;
        Ok((
            time,
            QuoteRow {
                bid: row.get(1)?.get_long()?,
                bid_qty: row.get(2)?.get_long()?,
                ask: row.get(3)?.get_long()?,
                ask_qty: row.get(4)?.get_long()?,
            },
        ))
    }
}

impl FeedBuilder for KdbFeed {
    fn run_mode(&self) -> RunMode {
        RunMode::HistoricalFrom(self.start)
    }

    fn run_for(&self) -> RunFor {
        RunFor::Duration(self.window)
    }

    fn wire(&self, g: &GraphBuilder) -> Result<Stream<Burst<BookUpdate>>> {
        let params = RunParams {
            run_mode: self.run_mode(),
            run_for: self.run_for(),
            start_time: self.start,
        };
        let rows = kdb_read::<QuoteRow>(
            g,
            params,
            self.connection.clone(),
            Duration::from_secs(30),
            |(t0, t1), _date, _iter| {
                format!(
                    "select time, bid, bidQty, ask, askQty from quotes \
                     where time >= (`timestamp$){}j, time < (`timestamp$){}j",
                    t0.to_kdb_timestamp(),
                    t1.to_kdb_timestamp(),
                )
            },
            None,
        )?;

        // Each row is a full top-of-book image. `recv_time` is the tick time —
        // under replay that *is* the recorded timestamp, which is exactly what
        // decision 2 of the vocabulary asks for; the store keeps no separate
        // venue clock, so it doubles as `venue_time`.
        let inst = InstrumentId::new("kdb", "EUR/USD");
        Ok(rows.with_time().map(move |(t, rows)| {
            let mut out = Burst::new();
            for row in rows.iter() {
                out.push(BookUpdate::Snapshot(BookSnapshot {
                    instrument: inst.clone(),
                    bids: vec![Level::new(px(row.bid), units(row.bid_qty))],
                    asks: vec![Level::new(px(row.ask), units(row.ask_qty))],
                    sequencing: Sequencing::None,
                    venue_time: Some(*t),
                    recv_time: *t,
                }));
            }
            out
        }))
    }
}

/// An integer pipette count, exactly, as a fixed-point price.
fn px(pipettes: i64) -> Px {
    Px::from_raw(pipettes as i128 * PIPETTE)
}

/// A whole-unit size, exactly, as a fixed-point quantity.
fn units(size: i64) -> Qty {
    Qty::from_raw(size as i128 * SCALE)
}

// ── Live: LMAX London demo over FIX/TLS ──────────────────────────────────────

const LMAX_MD_HOST: &str = "fix-marketdata.london-demo.lmax.com";
const LMAX_MD_PORT: u16 = 443;
const LMAX_MD_TARGET: &str = "LMXBDM";
/// LMAX instrument id for EUR/USD.
const EUR_USD_ID: &str = "4001";

const MSG_MARKET_DATA_SNAPSHOT: &str = "W";
const TAG_NO_MD_ENTRIES: u32 = 268;
const TAG_MD_ENTRY_TYPE: u32 = 269;
const TAG_MD_ENTRY_PX: u32 = 270;
const TAG_MD_ENTRY_SIZE: u32 = 271;

/// The same instrument, live: the LMAX London demo's FIX market-data session.
/// The session is live and unbounded, so this feed runs under
/// [`RunMode::RealTime`] — [`fix_connect_tls`] rejects a historical run at
/// wiring.
struct LmaxFeed {
    username: String,
    password: String,
    window: Duration,
}

impl FeedBuilder for LmaxFeed {
    fn run_mode(&self) -> RunMode {
        RunMode::RealTime
    }

    fn run_for(&self) -> RunFor {
        RunFor::Duration(self.window)
    }

    fn wire(&self, g: &GraphBuilder) -> Result<Stream<Burst<BookUpdate>>> {
        let fix = fix_connect_tls(
            g,
            self.run_mode(),
            LMAX_MD_HOST,
            LMAX_MD_PORT,
            &self.username,
            LMAX_MD_TARGET,
            Some(&self.password),
        )?;
        // Queues a MarketDataRequest for EUR/USD as soon as the session logs
        // in; session transitions go to the log rather than the book.
        let _sub = fix.fix_sub(g.constant(vec![EUR_USD_ID.to_string()]));
        let _status = fix.status.logged("lmax-status", log::Level::Info);

        // One subscription, so every snapshot on this session is ours and the
        // decoded updates all carry this one instrument. A second symbol would
        // need a demux on SecurityID (48) before the book — one instrument per
        // book is the vocabulary's rule, and mixing them aborts the run.
        let inst = InstrumentId::new("lmax", EUR_USD_ID);
        Ok(fix.data.with_time().try_map(move |(t, msgs)| {
            let mut out = Burst::new();
            for msg in msgs.iter() {
                if msg.msg_type == MSG_MARKET_DATA_SNAPSHOT {
                    out.push(decode_snapshot(&inst, *t, msg)?);
                }
            }
            Ok(out)
        }))
    }
}

/// Decode one LMAX `MarketDataSnapshotFullRefresh` (35=W) into a full book
/// image. Prices and sizes go through [`Px::parse`]/[`Qty::parse`] on the
/// venue's own decimal text — decision 1 of the vocabulary: no `f64` on the
/// parse path.
fn decode_snapshot(
    inst: &InstrumentId,
    recv_time: NanoTime,
    msg: &FixMessage,
) -> Result<BookUpdate> {
    let mut bids = Vec::new();
    let mut asks = Vec::new();
    for entry in msg.groups(TAG_NO_MD_ENTRIES, TAG_MD_ENTRY_TYPE) {
        let side = match entry.field(TAG_MD_ENTRY_TYPE) {
            Some("0") => Side::Bid,
            Some("1") => Side::Ask,
            // Trades, session highs/lows — present on some venues, not levels.
            _ => continue,
        };
        let price = entry
            .field(TAG_MD_ENTRY_PX)
            .context("MDEntry without MDEntryPx (270)")?;
        let size = entry
            .field(TAG_MD_ENTRY_SIZE)
            .context("MDEntry without MDEntrySize (271)")?;
        let level = Level::new(Px::parse(price)?, Qty::parse(size)?);
        match side {
            Side::Bid => bids.push(level),
            Side::Ask => asks.push(level),
        }
    }
    Ok(BookUpdate::Snapshot(BookSnapshot {
        instrument: inst.clone(),
        bids,
        asks,
        // Every message is a full refresh and LMAX numbers only the FIX
        // session, not the book, so there is no book sequence to check.
        sequencing: Sequencing::None,
        venue_time: (msg.sending_time != NanoTime::ZERO).then_some(msg.sending_time),
        recv_time,
    }))
}

// ── Shared output ────────────────────────────────────────────────────────────

/// What the strategy watches: the touch and the mid. `PartialEq` so
/// `distinct` emits only when the top actually moves.
#[derive(Clone, Debug, Default, PartialEq)]
struct TopOfBook {
    bid: Option<Level>,
    ask: Option<Level>,
    mid: Option<f64>,
}

impl TopOfBook {
    fn of(book: &OrderBook) -> Self {
        Self {
            bid: book.best_bid(),
            ask: book.best_ask(),
            mid: book.mid(),
        }
    }
}

impl fmt::Display for TopOfBook {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        fn side(f: &mut fmt::Formatter<'_>, name: &str, level: &Option<Level>) -> fmt::Result {
            match level {
                Some(l) => write!(f, "{name} {} @ {}", l.qty, l.price),
                None => write!(f, "{name} —"),
            }
        }
        side(f, "bid", &self.bid)?;
        write!(f, " | ")?;
        side(f, "ask", &self.ask)?;
        match self.mid {
            Some(mid) => write!(f, " | mid {mid:.6}"),
            None => write!(f, " | mid —"),
        }
    }
}

/// Wall-style `HH:MM:SS.mmm` for a graph timestamp — enough to line output up
/// against the recorded data without a date-time dependency.
fn clock(t: NanoTime) -> String {
    let ns: u64 = t.into();
    let s = ns / 1_000_000_000;
    let ms = ns % 1_000_000_000 / 1_000_000;
    format!(
        "{:02}:{:02}:{:02}.{ms:03}",
        s / 3600 % 24,
        s / 60 % 60,
        s % 60
    )
}
