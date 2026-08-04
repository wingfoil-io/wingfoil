//! Market data adapter — the **venue-neutral vocabulary** every market data
//! adapter produces, plus the order book state machine that consumes it.
//!
//! Unlike a messaging adapter, this one connects to nothing: it is the shared
//! type layer that venue adapters (Binance, Coinbase, Databento, …) normalise
//! *into*, so that a graph wired against one venue runs unchanged against
//! another. Venue adapters are **separate crates** rather than modules here:
//! each carries its own transport dependencies and release cadence, and wiring
//! one in costs this crate nothing. What lives here is the part they must all
//! agree on:
//!
//! - [`Px`] / [`Qty`] — fixed-point price and quantity, exact and orderable.
//! - [`InstrumentId`], [`Side`], [`Level`], [`LevelChange`] — the value types.
//! - [`Trade`], [`BookSnapshot`], [`BookDelta`], [`BookUpdate`],
//!   [`MarketEvent`] — the events an adapter emits.
//! - [`Sequencing`] — how a venue numbers its updates, normalised across the
//!   two shapes venues actually use.
//! - [`OrderBook`] — snapshot/delta book maintenance with gap detection, and
//!   [`MarketBookOps::order_book`], the op that maintains one on the graph.
//!
//! # Layering
//!
//! Following the [`stats`](crate::stats) module's pattern, the ops are *not*
//! in the [`prelude`](crate::prelude): bring the extension trait in explicitly
//! with `use wingfoil::adapters::market::MarketBookOps;`.
//!
//! # The four decisions an adapter must not make for itself
//!
//! These are the semantics that make two independently written adapters
//! interchangeable. An adapter that deviates is broken even if it compiles.
//!
//! **1. Prices and quantities are fixed-point, parsed from the venue's own
//! decimal text.** Venues send `"43210.10000000"` as a JSON *string*. Parse it
//! with [`Px::parse`], never via `f64`: a book keys levels *by price*, so a
//! delete arrives as "remove the level at 43210.10" and must match the key
//! exactly. Round-tripping through binary floating point is how that silently
//! stops matching. [`Px`] and [`Qty`] are `Ord + Eq + Hash` precisely because
//! `f64` is not.
//!
//! **2. Both timestamps are recorded, and they mean different things.**
//! [`venue_time`](BookSnapshot::venue_time) is the venue's own clock — absent
//! on venues that do not send one, and never trusted for ordering across
//! venues. [`recv_time`](BookSnapshot::recv_time) is engine time when the
//! adapter received the message, which is what the graph replays on. An
//! adapter sets `recv_time` from [`Ctx::time`](crate::op::Ctx::time), *not*
//! from [`NanoTime::now`] — that is what keeps a recorded session replayable.
//!
//! **3. A gap is signalled, never papered over.** When [`OrderBook::apply`]
//! detects a sequence discontinuity it clears the book, moves to
//! [`BookStatus::Gapped`] and reports [`BookApply::Gap`]. It does *not* keep
//! applying deltas to a book it knows is wrong. The adapter's obligation is to
//! notice and re-request a snapshot; downstream's obligation is to stop
//! trusting the book — which is why [`best_bid`](OrderBook::best_bid) and
//! friends return `None` while gapped, and why the op still ticks on a gap
//! rather than going quiet.
//!
//! **4. Deltas that arrive before the snapshot are buffered, not dropped.**
//! Every venue that serves a book as "REST snapshot + WebSocket deltas" has a
//! race: the stream is subscribed first, and its early messages predate the
//! snapshot. [`OrderBook`] buffers them ([`BookApply::Buffered`]), then on
//! snapshot discards the ones the snapshot already covers and replays the
//! rest. Dropping them instead leaves a book that is quietly missing its first
//! few updates.
//!
//! # Example
//!
//! Driving a book directly — which is also how an adapter's own tests should
//! exercise its normalisation, with no graph involved:
//!
//! ```
//! use wingfoil::NanoTime;
//! use wingfoil::adapters::market::{
//!     BookApply, BookDelta, BookSnapshot, BookStatus, BookUpdate, InstrumentId,
//!     Level, LevelChange, OrderBook, Px, Qty, Sequencing, Side,
//! };
//!
//! let inst = InstrumentId::new("example", "BTC-USD");
//! let mut book = OrderBook::new(inst.clone());
//!
//! book.apply(&BookUpdate::Snapshot(BookSnapshot {
//!     instrument: inst.clone(),
//!     bids: vec![Level::new(Px::parse("100.5")?, Qty::parse("2")?)],
//!     asks: vec![Level::new(Px::parse("101.0")?, Qty::parse("3")?)],
//!     sequencing: Sequencing::Single(7),
//!     venue_time: None,
//!     recv_time: NanoTime::ZERO,
//! }));
//! assert_eq!(book.status(), BookStatus::Live);
//! assert_eq!(book.mid(), Some(100.75));
//!
//! // A better bid takes the touch.
//! let outcome = book.apply(&BookUpdate::Delta(BookDelta {
//!     instrument: inst.clone(),
//!     changes: vec![LevelChange::new(Side::Bid, Px::parse("100.75")?, Qty::parse("1")?)],
//!     sequencing: Sequencing::Single(8),
//!     venue_time: None,
//!     recv_time: NanoTime::ZERO,
//! }));
//! assert_eq!(outcome, BookApply::Applied);
//! assert_eq!(book.best_bid().unwrap().price, Px::parse("100.75")?);
//! # Ok::<(), anyhow::Error>(())
//! ```
//!
//! On the graph, a `Stream<BookUpdate>` from a venue adapter becomes a stream
//! of maintained books with [`MarketBookOps::order_book`]:
//!
//! ```ignore
//! use wingfoil::adapters::market::{MarketBookOps, MarketEventOps};
//!
//! let books = venue_events.book_updates().order_book();
//! let mids = books.map_filter(|b| b.mid());
//! ```

use std::collections::BTreeMap;
use std::fmt;
use std::sync::Arc;

use anyhow::{Result, anyhow, bail};

use crate::Burst;
use crate::fluent::Stream;
use crate::op::{Activation, Ctx, Op, Tick};
use crate::runtime::time::NanoTime;
use wingfoil_derive::op;

// -------------------------------------------------------------------------
// Fixed-point price and quantity.
// -------------------------------------------------------------------------

/// Number of decimal places [`Px`] and [`Qty`] represent exactly.
pub const DECIMALS: u32 = 9;

/// The integer scale factor behind [`Px`] and [`Qty`]: `10^DECIMALS`.
pub const SCALE: i64 = 1_000_000_000;

/// Parse a plain decimal string (`-?digits[.digits]`) into a scaled integer.
///
/// Deliberately hand-rolled rather than routed through `f64`: the whole point
/// is to avoid a binary floating point round trip. Rejects exponent notation
/// and any fractional precision that would be lost, so a venue whose tick size
/// is finer than [`DECIMALS`] fails loudly at the adapter boundary instead of
/// silently corrupting book keys.
fn parse_fixed(s: &str) -> Result<i64> {
    let t = s.trim();
    if t.is_empty() {
        bail!("empty decimal string");
    }
    let (neg, digits) = match t.as_bytes()[0] {
        b'-' => (true, &t[1..]),
        b'+' => (false, &t[1..]),
        _ => (false, t),
    };
    if digits.is_empty() {
        bail!("decimal string {s:?} has a sign but no digits");
    }
    let (int_part, frac_part) = match digits.split_once('.') {
        Some((i, f)) => (i, f),
        None => (digits, ""),
    };
    // An empty integer part is fine (".5"), but the remaining text must be
    // digits only — this is what rejects "1e-9", "1_000" and "NaN".
    let all_digits = |p: &str| p.bytes().all(|b| b.is_ascii_digit());
    if !all_digits(int_part) || !all_digits(frac_part) {
        bail!("decimal string {s:?} is not a plain decimal number");
    }
    if int_part.is_empty() && frac_part.is_empty() {
        bail!("decimal string {s:?} has no digits");
    }

    let mut value: i64 = 0;
    for b in int_part.bytes() {
        value = value
            .checked_mul(10)
            .and_then(|v| v.checked_add((b - b'0') as i64))
            .ok_or_else(|| anyhow!("decimal string {s:?} overflows the fixed-point range"))?;
    }
    value = value
        .checked_mul(SCALE)
        .ok_or_else(|| anyhow!("decimal string {s:?} overflows the fixed-point range"))?;

    let mut scale = SCALE;
    for (i, b) in frac_part.bytes().enumerate() {
        let digit = (b - b'0') as i64;
        if i < DECIMALS as usize {
            scale /= 10;
            value = value
                .checked_add(digit * scale)
                .ok_or_else(|| anyhow!("decimal string {s:?} overflows the fixed-point range"))?;
        } else if digit != 0 {
            bail!(
                "decimal string {s:?} has more than {DECIMALS} decimal places of \
                 precision, which the fixed-point representation cannot hold \
                 exactly; the venue's tick size is finer than this adapter layer \
                 supports"
            );
        }
    }
    Ok(if neg { -value } else { value })
}

/// Render a scaled integer back to a plain decimal string, trimming trailing
/// fractional zeros (but never the whole fraction: `1.0` prints as `1`).
fn fmt_fixed(raw: i64, f: &mut fmt::Formatter<'_>) -> fmt::Result {
    let neg = raw < 0;
    // `unsigned_abs` rather than `abs` so `i64::MIN` does not panic.
    let mag = raw.unsigned_abs();
    let int = mag / SCALE as u64;
    let frac = mag % SCALE as u64;
    if neg {
        write!(f, "-")?;
    }
    if frac == 0 {
        write!(f, "{int}")
    } else {
        let s = format!("{frac:0width$}", width = DECIMALS as usize);
        write!(f, "{int}.{}", s.trim_end_matches('0'))
    }
}

macro_rules! fixed_point {
    ($name:ident, $what:literal) => {
        #[doc = concat!("A ", $what, " as a fixed-point integer with [`DECIMALS`] decimal places.")]
        ///
        /// `Ord`, `Eq` and `Hash` — the properties `f64` lacks and a book keyed
        /// by price needs. Construct with [`parse`](Self::parse) from the
        /// venue's own decimal text wherever possible; [`from_f64`](Self::from_f64)
        /// rounds and is a lossy convenience for tests and display code.
        ///
        /// Range is `±9_223_372_036.854_775_807`. A venue whose values exceed
        /// that (or whose tick size is finer than [`DECIMALS`]) is out of scope
        /// for this representation, and [`parse`](Self::parse) says so rather
        /// than truncating.
        #[derive(Clone, Copy, Debug, Default, PartialEq, Eq, PartialOrd, Ord, Hash)]
        pub struct $name(i64);

        impl $name {
            #[doc = concat!("The zero ", $what, ".")]
            pub const ZERO: Self = Self(0);

            /// Wrap a raw scaled integer (already multiplied by [`SCALE`]).
            pub const fn from_raw(raw: i64) -> Self {
                Self(raw)
            }

            /// The underlying scaled integer.
            pub const fn raw(self) -> i64 {
                self.0
            }

            /// Parse the venue's decimal text exactly, with no `f64` round trip.
            pub fn parse(s: &str) -> Result<Self> {
                parse_fixed(s).map(Self)
            }

            /// Round an `f64` into fixed point. Lossy by nature — prefer
            /// [`parse`](Self::parse) on anything that came off a wire.
            pub fn from_f64(v: f64) -> Self {
                Self((v * SCALE as f64).round() as i64)
            }

            /// Convert to `f64`, for arithmetic and the `f64`-typed
            /// [`stats`](crate::stats) ops. Lossy above 2^53 raw units.
            pub fn to_f64(self) -> f64 {
                self.0 as f64 / SCALE as f64
            }

            #[doc = concat!("Whether this ", $what, " is zero.")]
            pub const fn is_zero(self) -> bool {
                self.0 == 0
            }
        }

        impl fmt::Display for $name {
            fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                fmt_fixed(self.0, f)
            }
        }
    };
}

fixed_point!(Px, "price");
fixed_point!(Qty, "quantity");

// -------------------------------------------------------------------------
// Value types.
// -------------------------------------------------------------------------

/// Which side of the book a level or trade sits on.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Side {
    /// A buy: bids, sorted best (highest) first.
    Bid,
    /// A sell: asks, sorted best (lowest) first.
    Ask,
}

impl Side {
    /// The opposing side.
    pub const fn opposite(self) -> Side {
        match self {
            Side::Bid => Side::Ask,
            Side::Ask => Side::Bid,
        }
    }
}

/// A venue-qualified instrument.
///
/// Both fields are `Arc<str>` so the id can ride on every event without a
/// per-tick allocation — an adapter builds one per subscription and clones it
/// into each message.
///
/// The `Default` impl (an empty venue and symbol) exists only because the
/// engine requires every stream's value type to be `Default` for its
/// pre-first-tick value slot. It is not a meaningful instrument.
#[derive(Clone, Debug, Default, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct InstrumentId {
    /// The venue's short name, lowercase by convention (`"binance"`).
    pub venue: Arc<str>,
    /// The venue's own symbol, verbatim and *not* normalised — venues disagree
    /// about separators and casing, and rewriting the symbol loses the ability
    /// to echo it back on a subscribe.
    pub symbol: Arc<str>,
}

impl InstrumentId {
    /// Build an instrument id from a venue name and the venue's own symbol.
    pub fn new(venue: impl AsRef<str>, symbol: impl AsRef<str>) -> Self {
        Self {
            venue: Arc::from(venue.as_ref()),
            symbol: Arc::from(symbol.as_ref()),
        }
    }
}

impl fmt::Display for InstrumentId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}:{}", self.venue, self.symbol)
    }
}

/// One price level of a book: a price and the total quantity resting there.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Level {
    /// The level's price.
    pub price: Px,
    /// Total resting quantity at that price. Never zero in a materialised
    /// book — a zero quantity is a *deletion*, see [`LevelChange`].
    pub qty: Qty,
}

impl Level {
    /// Build a level.
    pub const fn new(price: Px, qty: Qty) -> Self {
        Self { price, qty }
    }
}

/// A single level mutation carried by a [`BookDelta`].
///
/// Venues almost universally express book updates as *absolute* replacement —
/// "the quantity at this price is now X" — with `X == 0` meaning "remove this
/// level". That convention is preserved here rather than split into separate
/// set/delete variants, because an adapter translating a venue message would
/// otherwise have to branch on a value the book is about to branch on anyway.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct LevelChange {
    /// Which side the level belongs to.
    pub side: Side,
    /// The level's price.
    pub price: Px,
    /// The new absolute resting quantity, or zero to remove the level.
    pub qty: Qty,
}

impl LevelChange {
    /// Build a level change.
    pub const fn new(side: Side, price: Px, qty: Qty) -> Self {
        Self { side, price, qty }
    }

    /// Whether this change removes the level.
    pub const fn is_removal(&self) -> bool {
        self.qty.is_zero()
    }
}

// -------------------------------------------------------------------------
// Sequencing.
// -------------------------------------------------------------------------

/// How a venue numbers the updates on a book channel — normalised across the
/// two shapes that exist in the wild.
///
/// This is the field that makes gap detection portable. An adapter maps the
/// venue's own scheme onto one of these variants and the book does the rest.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum Sequencing {
    /// The venue provides no usable sequence number, so gaps are undetectable.
    ///
    /// Every update is accepted in arrival order. Choose this only when the
    /// venue genuinely sends nothing — it silently forfeits the guarantee that
    /// the rest of this module exists to provide.
    #[default]
    None,
    /// One monotonically increasing number per message, incrementing by
    /// exactly one (Coinbase-style). The next message must be `n + 1`.
    Single(u64),
    /// An inclusive span of update ids covered by this message
    /// (Binance-style `U`/`u`). The next message must start at `last + 1`.
    Span {
        /// First update id covered, inclusive.
        first: u64,
        /// Last update id covered, inclusive.
        last: u64,
    },
}

impl Sequencing {
    /// The first id this update covers, if any.
    pub const fn first(self) -> Option<u64> {
        match self {
            Sequencing::None => None,
            Sequencing::Single(n) => Some(n),
            Sequencing::Span { first, .. } => Some(first),
        }
    }

    /// The last id this update covers, if any.
    pub const fn last(self) -> Option<u64> {
        match self {
            Sequencing::None => None,
            Sequencing::Single(n) => Some(n),
            Sequencing::Span { last, .. } => Some(last),
        }
    }
}

// -------------------------------------------------------------------------
// Events.
// -------------------------------------------------------------------------

/// A public trade print.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Trade {
    /// The instrument traded.
    pub instrument: InstrumentId,
    /// Execution price.
    pub price: Px,
    /// Executed quantity, always positive — direction is [`aggressor`](Self::aggressor).
    pub qty: Qty,
    /// Which side crossed the spread, when the venue reveals it.
    pub aggressor: Option<Side>,
    /// The venue's own trade id, verbatim, when it sends one.
    pub trade_id: Option<Arc<str>>,
    /// The venue's clock. `None` when the venue sends no timestamp.
    pub venue_time: Option<NanoTime>,
    /// Engine time when the adapter received this message. See the module
    /// docs — this comes from [`Ctx::time`](crate::op::Ctx::time), never from
    /// [`NanoTime::now`].
    pub recv_time: NanoTime,
}

/// A full replacement image of one instrument's book.
///
/// `bids` and `asks` are supplied best-first by convention, but [`OrderBook`]
/// re-sorts on apply and does not rely on it.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct BookSnapshot {
    /// The instrument this image is for.
    pub instrument: InstrumentId,
    /// Resting bids.
    pub bids: Vec<Level>,
    /// Resting asks.
    pub asks: Vec<Level>,
    /// The update id this image is current as of.
    pub sequencing: Sequencing,
    /// The venue's clock, when it sends one.
    pub venue_time: Option<NanoTime>,
    /// Engine time when the adapter received this message.
    pub recv_time: NanoTime,
}

/// An incremental book update: a batch of level replacements.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct BookDelta {
    /// The instrument this update is for.
    pub instrument: InstrumentId,
    /// The level mutations, applied in order.
    pub changes: Vec<LevelChange>,
    /// The update id(s) this message covers.
    pub sequencing: Sequencing,
    /// The venue's clock, when it sends one.
    pub venue_time: Option<NanoTime>,
    /// Engine time when the adapter received this message.
    pub recv_time: NanoTime,
}

/// Either kind of book message — what [`MarketBookOps::order_book`] consumes.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum BookUpdate {
    /// A full image.
    Snapshot(BookSnapshot),
    /// An incremental update.
    Delta(BookDelta),
}

/// Only to satisfy the engine's requirement that a stream's value type be
/// `Default` for its pre-first-tick slot — an empty snapshot, not a meaningful
/// update.
impl Default for BookUpdate {
    fn default() -> Self {
        BookUpdate::Snapshot(BookSnapshot::default())
    }
}

impl BookUpdate {
    /// The instrument this update is for.
    pub fn instrument(&self) -> &InstrumentId {
        match self {
            BookUpdate::Snapshot(s) => &s.instrument,
            BookUpdate::Delta(d) => &d.instrument,
        }
    }

    /// The update id(s) this message covers.
    pub fn sequencing(&self) -> Sequencing {
        match self {
            BookUpdate::Snapshot(s) => s.sequencing,
            BookUpdate::Delta(d) => d.sequencing,
        }
    }

    /// Engine time when the adapter received this message.
    pub fn recv_time(&self) -> NanoTime {
        match self {
            BookUpdate::Snapshot(s) => s.recv_time,
            BookUpdate::Delta(d) => d.recv_time,
        }
    }

    /// The venue's clock, when it sent one.
    pub fn venue_time(&self) -> Option<NanoTime> {
        match self {
            BookUpdate::Snapshot(s) => s.venue_time,
            BookUpdate::Delta(d) => d.venue_time,
        }
    }
}

/// Any normalised market data event — the single stream type an adapter
/// multiplexing one venue connection emits.
///
/// A graph that wants one kind demultiplexes with
/// [`map_filter`](crate::fluent::StreamOps::map_filter); see
/// [`MarketEventOps`].
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum MarketEvent {
    /// A public trade print.
    Trade(Trade),
    /// A book snapshot or delta.
    Book(BookUpdate),
}

/// As for [`BookUpdate`]: the engine's value-slot requirement, not a
/// meaningful event.
impl Default for MarketEvent {
    fn default() -> Self {
        MarketEvent::Book(BookUpdate::default())
    }
}

impl MarketEvent {
    /// The instrument this event is for.
    pub fn instrument(&self) -> &InstrumentId {
        match self {
            MarketEvent::Trade(t) => &t.instrument,
            MarketEvent::Book(b) => b.instrument(),
        }
    }

    /// Engine time when the adapter received this message.
    pub fn recv_time(&self) -> NanoTime {
        match self {
            MarketEvent::Trade(t) => t.recv_time,
            MarketEvent::Book(b) => b.recv_time(),
        }
    }
}

// -------------------------------------------------------------------------
// Order book.
// -------------------------------------------------------------------------

/// Where a book is in its snapshot/delta lifecycle.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum BookStatus {
    /// No snapshot applied yet. Deltas are being buffered.
    #[default]
    AwaitingSnapshot,
    /// The book is current and safe to quote off.
    Live,
    /// A sequence gap was detected. The book has been cleared and every
    /// further delta is refused until a fresh snapshot arrives.
    Gapped,
}

/// The upper bound on deltas buffered while awaiting a snapshot.
///
/// A snapshot that never arrives would otherwise buffer without limit. On
/// overflow the book moves to [`BookStatus::Gapped`], which is the honest
/// outcome: the adapter is not going to be able to build a correct book from
/// what it has.
pub const MAX_BUFFERED_DELTAS: usize = 16_384;

/// What [`OrderBook::apply`] did with an update.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BookApply {
    /// The book advanced.
    Applied,
    /// The update predates the current image and was discarded. Normal during
    /// the snapshot race; not an error.
    Stale,
    /// Buffered pending a snapshot.
    Buffered,
    /// A sequence discontinuity. The book has been cleared and is now
    /// [`BookStatus::Gapped`]; the adapter must re-request a snapshot.
    Gap {
        /// The update id the book expected next.
        expected: u64,
        /// The first update id the message actually carried.
        got: u64,
    },
}

/// A snapshot/delta order book with sequence-gap detection.
///
/// Maintained on the graph by [`MarketBookOps::order_book`], but usable
/// standalone — an adapter's tests should drive one directly.
///
/// Levels live in `BTreeMap`s keyed by [`Px`], so best-of-book is the first
/// entry on the ask side and the last on the bid side, and a delete is an
/// exact key removal (see decision 1 in the module docs).
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct OrderBook {
    instrument: InstrumentId,
    bids: BTreeMap<Px, Qty>,
    asks: BTreeMap<Px, Qty>,
    status: BookStatus,
    last_seq: Option<u64>,
    pending: Vec<BookDelta>,
    venue_time: Option<NanoTime>,
    recv_time: NanoTime,
}

impl OrderBook {
    /// An empty book awaiting its first snapshot.
    pub fn new(instrument: InstrumentId) -> Self {
        Self {
            instrument,
            bids: BTreeMap::new(),
            asks: BTreeMap::new(),
            status: BookStatus::AwaitingSnapshot,
            last_seq: None,
            pending: Vec::new(),
            venue_time: None,
            recv_time: NanoTime::ZERO,
        }
    }

    /// The instrument this book tracks.
    pub fn instrument(&self) -> &InstrumentId {
        &self.instrument
    }

    /// Where the book is in its lifecycle.
    pub fn status(&self) -> BookStatus {
        self.status
    }

    /// Whether the book is current and safe to quote off.
    pub fn is_live(&self) -> bool {
        self.status == BookStatus::Live
    }

    /// The last update id applied, if the venue sequences its updates.
    pub fn last_sequence(&self) -> Option<u64> {
        self.last_seq
    }

    /// The venue clock of the most recently applied update.
    pub fn venue_time(&self) -> Option<NanoTime> {
        self.venue_time
    }

    /// Engine time of the most recently applied update.
    pub fn recv_time(&self) -> NanoTime {
        self.recv_time
    }

    /// Apply a snapshot or delta, returning what happened.
    ///
    /// See the module docs for the buffering and gap contracts — this method
    /// is where both are implemented.
    pub fn apply(&mut self, update: &BookUpdate) -> BookApply {
        match update {
            BookUpdate::Snapshot(s) => self.apply_snapshot(s),
            BookUpdate::Delta(d) => self.apply_delta(d),
        }
    }

    fn apply_snapshot(&mut self, s: &BookSnapshot) -> BookApply {
        self.bids.clear();
        self.asks.clear();
        for lvl in &s.bids {
            if !lvl.qty.is_zero() {
                self.bids.insert(lvl.price, lvl.qty);
            }
        }
        for lvl in &s.asks {
            if !lvl.qty.is_zero() {
                self.asks.insert(lvl.price, lvl.qty);
            }
        }
        self.last_seq = s.sequencing.last();
        self.status = BookStatus::Live;
        self.venue_time = s.venue_time;
        self.recv_time = s.recv_time;

        // Replay whatever arrived while we were waiting for this image. Taking
        // the buffer first means a gap discovered mid-replay leaves nothing
        // stale behind for the *next* snapshot to replay a second time.
        let pending = std::mem::take(&mut self.pending);
        for d in &pending {
            match self.apply_delta(d) {
                BookApply::Gap { expected, got } => return BookApply::Gap { expected, got },
                _ => continue,
            }
        }
        BookApply::Applied
    }

    fn apply_delta(&mut self, d: &BookDelta) -> BookApply {
        match self.status {
            BookStatus::AwaitingSnapshot => {
                if self.pending.len() >= MAX_BUFFERED_DELTAS {
                    self.gap_out();
                    // No sequence numbers are necessarily involved, so report
                    // the overflow against whatever the message carried.
                    let got = d.sequencing.first().unwrap_or(0);
                    return BookApply::Gap { expected: got, got };
                }
                self.pending.push(d.clone());
                BookApply::Buffered
            }
            // A gapped book refuses deltas outright: applying them would build
            // on an image already known to be wrong.
            BookStatus::Gapped => BookApply::Stale,
            BookStatus::Live => {
                if let Some(last_applied) = self.last_seq {
                    let (first, last) = match d.sequencing {
                        // The venue does not sequence, so there is nothing to
                        // check — accept in arrival order.
                        Sequencing::None => {
                            self.apply_changes(d);
                            return BookApply::Applied;
                        }
                        Sequencing::Single(n) => (n, n),
                        Sequencing::Span { first, last } => (first, last),
                    };
                    // Wholly covered by the image we already hold.
                    if last <= last_applied {
                        return BookApply::Stale;
                    }
                    // A span that straddles the snapshot is exactly the
                    // Binance handoff case: `first <= last_applied + 1 <= last`
                    // is contiguous, anything higher has lost messages.
                    if first > last_applied + 1 {
                        self.gap_out();
                        return BookApply::Gap {
                            expected: last_applied + 1,
                            got: first,
                        };
                    }
                    self.apply_changes(d);
                    self.last_seq = Some(last);
                    BookApply::Applied
                } else {
                    // Live with no sequence baseline — the snapshot carried
                    // `Sequencing::None`. Adopt whatever this delta reports so
                    // detection starts as soon as the venue provides numbers.
                    self.apply_changes(d);
                    self.last_seq = d.sequencing.last();
                    BookApply::Applied
                }
            }
        }
    }

    fn apply_changes(&mut self, d: &BookDelta) {
        for c in &d.changes {
            let side = match c.side {
                Side::Bid => &mut self.bids,
                Side::Ask => &mut self.asks,
            };
            if c.is_removal() {
                side.remove(&c.price);
            } else {
                side.insert(c.price, c.qty);
            }
        }
        self.venue_time = d.venue_time;
        self.recv_time = d.recv_time;
    }

    /// Clear the book and mark it gapped. Clearing (rather than leaving the
    /// stale levels visible) is what makes `best_bid()` return `None`, so a
    /// downstream that forgot to check [`status`](Self::status) still fails
    /// safe instead of quoting off a wrong book.
    fn gap_out(&mut self) {
        self.bids.clear();
        self.asks.clear();
        self.pending.clear();
        self.last_seq = None;
        self.status = BookStatus::Gapped;
    }

    /// Best bid — the highest-priced resting buy. `None` when that side is
    /// empty or the book is not [`BookStatus::Live`].
    pub fn best_bid(&self) -> Option<Level> {
        if !self.is_live() {
            return None;
        }
        self.bids
            .iter()
            .next_back()
            .map(|(&price, &qty)| Level::new(price, qty))
    }

    /// Best ask — the lowest-priced resting sell. `None` when that side is
    /// empty or the book is not [`BookStatus::Live`].
    pub fn best_ask(&self) -> Option<Level> {
        if !self.is_live() {
            return None;
        }
        self.asks
            .iter()
            .next()
            .map(|(&price, &qty)| Level::new(price, qty))
    }

    /// The `n` best levels of one side, best first.
    pub fn depth(&self, side: Side, n: usize) -> Vec<Level> {
        if !self.is_live() {
            return Vec::new();
        }
        match side {
            Side::Bid => self
                .bids
                .iter()
                .rev()
                .take(n)
                .map(|(&price, &qty)| Level::new(price, qty))
                .collect(),
            Side::Ask => self
                .asks
                .iter()
                .take(n)
                .map(|(&price, &qty)| Level::new(price, qty))
                .collect(),
        }
    }

    /// Number of distinct price levels on one side.
    pub fn level_count(&self, side: Side) -> usize {
        match side {
            Side::Bid => self.bids.len(),
            Side::Ask => self.asks.len(),
        }
    }

    /// Mid price, as `f64` for the [`stats`](crate::stats) ops. `None` unless
    /// the book is live and two-sided.
    pub fn mid(&self) -> Option<f64> {
        let (b, a) = (self.best_bid()?, self.best_ask()?);
        Some((b.price.to_f64() + a.price.to_f64()) / 2.0)
    }

    /// Ask minus bid. `None` unless the book is live and two-sided.
    pub fn spread(&self) -> Option<Px> {
        let (b, a) = (self.best_bid()?, self.best_ask()?);
        Some(Px::from_raw(a.price.raw() - b.price.raw()))
    }

    /// Mid weighted by the quantity resting at each touch — the standard
    /// microprice. `None` unless the book is live and two-sided.
    pub fn microprice(&self) -> Option<f64> {
        let (b, a) = (self.best_bid()?, self.best_ask()?);
        let (bq, aq) = (b.qty.to_f64(), a.qty.to_f64());
        let total = bq + aq;
        if total <= 0.0 {
            return None;
        }
        // Weighted *towards* the side with less size resting: the touch with
        // the smaller queue is the one more likely to trade through.
        Some((b.price.to_f64() * aq + a.price.to_f64() * bq) / total)
    }
}

// -------------------------------------------------------------------------
// The op.
// -------------------------------------------------------------------------

/// Maintains an [`OrderBook`] from a stream of [`BookUpdate`]s.
///
/// The book is held as an `Arc` and mutated through `Arc::make_mut`, so a
/// cycle whose output nobody retained mutates in place and a downstream that
/// keeps one gets a cheap copy-on-write snapshot.
pub struct OrderBookOp;

#[op(build = order_book)]
impl Op for OrderBookOp {
    type Cfg = ();
    type State = Option<Arc<OrderBook>>;
    type In<'a> = (&'a BookUpdate,);
    type Out = Arc<OrderBook>;
    const ACTIVATION: Activation = Activation::NONE;

    fn cycle(
        _cfg: &mut (),
        state: &mut Option<Arc<OrderBook>>,
        input: (&BookUpdate,),
        _ctx: &mut Ctx<'_>,
    ) -> Result<Tick<Arc<OrderBook>>> {
        let update = input.0;
        let book = book_for(state, update)?;
        let outcome = Arc::make_mut(book).apply(update);
        Ok(match outcome {
            // A gap ticks too: downstream must learn that the book went
            // invalid, and silence would leave it quoting off the last good
            // value indefinitely.
            BookApply::Applied | BookApply::Gap { .. } => Tick::Value(Arc::clone(book)),
            BookApply::Stale | BookApply::Buffered => Tick::Quiet,
        })
    }
}

/// Maintains an [`OrderBook`] from a stream of same-instant [`Burst`]s.
///
/// The burst form is the one an adapter's source actually produces, and it is
/// where "never latest-wins" earns its keep: a book must apply *every* update
/// in the group in order. Collapsing a burst to its last value would drop the
/// intervening level changes and silently desynchronise the book.
///
/// One tick per burst, carrying the book after the whole group is applied.
pub struct OrderBookBurstOp;

#[op(build = order_book_bursts)]
impl Op for OrderBookBurstOp {
    type Cfg = ();
    type State = Option<Arc<OrderBook>>;
    type In<'a> = (&'a Burst<BookUpdate>,);
    type Out = Arc<OrderBook>;
    const ACTIVATION: Activation = Activation::NONE;

    fn cycle(
        _cfg: &mut (),
        state: &mut Option<Arc<OrderBook>>,
        input: (&Burst<BookUpdate>,),
        _ctx: &mut Ctx<'_>,
    ) -> Result<Tick<Arc<OrderBook>>> {
        let mut advanced = false;
        for update in input.0.iter() {
            let book = book_for(state, update)?;
            match Arc::make_mut(book).apply(update) {
                BookApply::Applied | BookApply::Gap { .. } => advanced = true,
                BookApply::Stale | BookApply::Buffered => {}
            }
        }
        Ok(match state {
            Some(book) if advanced => Tick::Value(Arc::clone(book)),
            _ => Tick::Quiet,
        })
    }
}

/// Resolve the book a given update belongs to, creating it on first sight.
///
/// The instrument comes from the data rather than a config argument, so the
/// common one-instrument-per-stream wiring needs no ceremony. A mixed stream is
/// a wiring bug, not a runtime condition — fail loudly rather than silently
/// interleaving two venues into one book.
fn book_for<'s>(
    state: &'s mut Option<Arc<OrderBook>>,
    update: &BookUpdate,
) -> Result<&'s mut Arc<OrderBook>> {
    let book = state.get_or_insert_with(|| Arc::new(OrderBook::new(update.instrument().clone())));
    if book.instrument() != update.instrument() {
        bail!(
            "order_book received an update for {} on a book tracking {}; \
             demultiplex by instrument before wiring order_book",
            update.instrument(),
            book.instrument()
        );
    }
    Ok(book)
}

/// Book maintenance on a stream of normalised book messages.
///
/// Not in the [`prelude`](crate::prelude) — `use
/// wingfoil::adapters::market::MarketBookOps;`.
pub trait MarketBookOps {
    /// Maintain an [`OrderBook`] from this stream of updates.
    ///
    /// Ticks whenever the book advances *or* goes invalid — check
    /// [`OrderBook::status`], or rely on [`best_bid`](OrderBook::best_bid)
    /// returning `None` while gapped. Stays quiet for updates that were
    /// buffered pending a snapshot or discarded as stale.
    ///
    /// The stream must carry one instrument; a second one is an error that
    /// aborts the run.
    fn order_book(&self) -> Stream<Arc<OrderBook>>;
}

impl MarketBookOps for Stream<BookUpdate> {
    fn order_book(&self) -> Stream<Arc<OrderBook>> {
        self.wire(|b, h| b.order_book(h))
    }
}

impl MarketBookOps for Stream<Burst<BookUpdate>> {
    fn order_book(&self) -> Stream<Arc<OrderBook>> {
        self.wire(|b, h| b.order_book_bursts(h))
    }
}

/// Demultiplexing a combined [`MarketEvent`] stream into its parts.
///
/// Not in the [`prelude`](crate::prelude) — `use
/// wingfoil::adapters::market::MarketEventOps;`.
pub trait MarketEventOps {
    /// Just the trade prints.
    fn trades(&self) -> Stream<Trade>;
    /// Just the book messages — wire this into
    /// [`order_book`](MarketBookOps::order_book).
    fn book_updates(&self) -> Stream<BookUpdate>;
}

impl MarketEventOps for Stream<MarketEvent> {
    fn trades(&self) -> Stream<Trade> {
        use crate::fluent::StreamOps;
        self.map_filter(|e| match e {
            MarketEvent::Trade(t) => (t.clone(), true),
            _ => (Trade::default(), false),
        })
    }

    fn book_updates(&self) -> Stream<BookUpdate> {
        use crate::fluent::StreamOps;
        self.map_filter(|e| match e {
            MarketEvent::Book(b) => (b.clone(), true),
            _ => (BookUpdate::default(), false),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn inst() -> InstrumentId {
        InstrumentId::new("test", "BTC-USD")
    }

    fn snapshot(seq: Sequencing, bids: &[(&str, &str)], asks: &[(&str, &str)]) -> BookUpdate {
        BookUpdate::Snapshot(BookSnapshot {
            instrument: inst(),
            bids: bids
                .iter()
                .map(|(p, q)| Level::new(Px::parse(p).unwrap(), Qty::parse(q).unwrap()))
                .collect(),
            asks: asks
                .iter()
                .map(|(p, q)| Level::new(Px::parse(p).unwrap(), Qty::parse(q).unwrap()))
                .collect(),
            sequencing: seq,
            venue_time: None,
            recv_time: NanoTime::ZERO,
        })
    }

    fn delta(seq: Sequencing, changes: &[(Side, &str, &str)]) -> BookUpdate {
        BookUpdate::Delta(BookDelta {
            instrument: inst(),
            changes: changes
                .iter()
                .map(|(s, p, q)| {
                    LevelChange::new(*s, Px::parse(p).unwrap(), Qty::parse(q).unwrap())
                })
                .collect(),
            sequencing: seq,
            venue_time: None,
            recv_time: NanoTime::ZERO,
        })
    }

    // --- fixed point -----------------------------------------------------

    #[test]
    fn parses_venue_decimal_text_exactly() {
        assert_eq!(
            Px::parse("43210.10000000").unwrap().raw(),
            43_210_100_000_000
        );
        assert_eq!(Px::parse("0.000000001").unwrap().raw(), 1);
        assert_eq!(Px::parse("1").unwrap().raw(), SCALE);
        assert_eq!(Px::parse(".5").unwrap().raw(), SCALE / 2);
        assert_eq!(Px::parse("-2.25").unwrap().raw(), -2_250_000_000);
        assert_eq!(Px::parse("  7.5  ").unwrap().raw(), 7_500_000_000);
    }

    #[test]
    fn parse_rejects_what_it_cannot_represent() {
        // More precision than the scale holds — must fail, never truncate.
        assert!(Px::parse("0.0000000001").is_err());
        // Trailing zeros beyond the scale are not a loss, so they are fine.
        assert_eq!(Px::parse("1.5000000000000").unwrap().raw(), 1_500_000_000);
        assert!(Px::parse("1e-9").is_err());
        assert!(Px::parse("abc").is_err());
        assert!(Px::parse("").is_err());
        assert!(Px::parse("-").is_err());
        assert!(Px::parse("1_000").is_err());
        assert!(Px::parse("99999999999999999999").is_err());
    }

    #[test]
    fn display_round_trips_through_parse() {
        for s in ["43210.1", "0.000000001", "1", "-2.25", "0"] {
            assert_eq!(Px::parse(s).unwrap().to_string(), s);
        }
        // Trailing zeros are trimmed, so the text normalises.
        assert_eq!(Px::parse("1.5000").unwrap().to_string(), "1.5");
    }

    #[test]
    fn prices_that_f64_would_collide_stay_distinct() {
        // The point of fixed point: these are exact, ordered keys.
        let a = Px::parse("0.1").unwrap();
        let b = Px::parse("0.2").unwrap();
        let c = Px::parse("0.3").unwrap();
        assert_eq!(Px::from_raw(a.raw() + b.raw()), c);
        assert!(a < b && b < c);
    }

    // --- book maintenance ------------------------------------------------

    #[test]
    fn snapshot_then_delta_maintains_touch() {
        let mut book = OrderBook::new(inst());
        assert_eq!(book.status(), BookStatus::AwaitingSnapshot);

        let r = book.apply(&snapshot(
            Sequencing::Single(10),
            &[("100.5", "2"), ("100.0", "5")],
            &[("101.0", "3")],
        ));
        assert_eq!(r, BookApply::Applied);
        assert_eq!(book.status(), BookStatus::Live);
        assert_eq!(
            book.best_bid(),
            Some(Level::new(
                Px::parse("100.5").unwrap(),
                Qty::parse("2").unwrap()
            ))
        );
        assert_eq!(
            book.best_ask(),
            Some(Level::new(
                Px::parse("101.0").unwrap(),
                Qty::parse("3").unwrap()
            ))
        );

        // A better bid takes the touch.
        let r = book.apply(&delta(
            Sequencing::Single(11),
            &[(Side::Bid, "100.75", "1")],
        ));
        assert_eq!(r, BookApply::Applied);
        assert_eq!(book.best_bid().unwrap().price, Px::parse("100.75").unwrap());
        assert_eq!(book.last_sequence(), Some(11));
    }

    #[test]
    fn zero_quantity_removes_the_level() {
        let mut book = OrderBook::new(inst());
        book.apply(&snapshot(
            Sequencing::Single(1),
            &[("100.5", "2"), ("100.0", "5")],
            &[("101.0", "3")],
        ));
        assert_eq!(book.level_count(Side::Bid), 2);

        book.apply(&delta(Sequencing::Single(2), &[(Side::Bid, "100.5", "0")]));
        assert_eq!(book.level_count(Side::Bid), 1);
        assert_eq!(book.best_bid().unwrap().price, Px::parse("100.0").unwrap());

        // A snapshot level with zero quantity is likewise not materialised.
        book.apply(&snapshot(Sequencing::Single(3), &[("99.0", "0")], &[]));
        assert_eq!(book.level_count(Side::Bid), 0);
    }

    #[test]
    fn gap_clears_the_book_and_refuses_further_deltas() {
        let mut book = OrderBook::new(inst());
        book.apply(&snapshot(
            Sequencing::Single(10),
            &[("100.0", "1")],
            &[("101.0", "1")],
        ));

        // 11 is expected; 13 means 11 and 12 were lost.
        let r = book.apply(&delta(Sequencing::Single(13), &[(Side::Bid, "100.5", "1")]));
        assert_eq!(
            r,
            BookApply::Gap {
                expected: 11,
                got: 13
            }
        );
        assert_eq!(book.status(), BookStatus::Gapped);

        // Fails safe: the touch reads empty even though levels were once known.
        assert_eq!(book.best_bid(), None);
        assert_eq!(book.best_ask(), None);
        assert_eq!(book.mid(), None);
        assert!(book.depth(Side::Bid, 5).is_empty());

        // Further deltas are refused rather than applied to a wrong image.
        let r = book.apply(&delta(Sequencing::Single(14), &[(Side::Bid, "100.5", "1")]));
        assert_eq!(r, BookApply::Stale);
        assert_eq!(book.status(), BookStatus::Gapped);

        // A fresh snapshot recovers.
        let r = book.apply(&snapshot(
            Sequencing::Single(20),
            &[("100.0", "1")],
            &[("101.0", "1")],
        ));
        assert_eq!(r, BookApply::Applied);
        assert_eq!(book.status(), BookStatus::Live);
        assert_eq!(book.best_bid().unwrap().price, Px::parse("100.0").unwrap());
    }

    #[test]
    fn stale_deltas_are_discarded_not_applied() {
        let mut book = OrderBook::new(inst());
        book.apply(&snapshot(Sequencing::Single(10), &[("100.0", "1")], &[]));
        let r = book.apply(&delta(Sequencing::Single(9), &[(Side::Bid, "50.0", "1")]));
        assert_eq!(r, BookApply::Stale);
        assert_eq!(book.level_count(Side::Bid), 1);
        assert_eq!(book.last_sequence(), Some(10));
    }

    #[test]
    fn pre_snapshot_deltas_are_buffered_then_replayed() {
        let mut book = OrderBook::new(inst());

        // The Binance handoff: the stream is subscribed before the REST
        // snapshot lands, so these arrive first.
        for seq in 5..=8 {
            let r = book.apply(&delta(
                Sequencing::Span {
                    first: seq,
                    last: seq,
                },
                &[(Side::Bid, "100.0", &format!("{seq}"))],
            ));
            assert_eq!(r, BookApply::Buffered);
        }

        // Snapshot is current as of 6, so 5 and 6 are dropped and 7, 8 replay.
        let r = book.apply(&snapshot(Sequencing::Span { first: 6, last: 6 }, &[], &[]));
        assert_eq!(r, BookApply::Applied);
        assert_eq!(book.status(), BookStatus::Live);
        assert_eq!(book.last_sequence(), Some(8));
        // The last replayed delta won.
        assert_eq!(book.best_bid().unwrap().qty, Qty::parse("8").unwrap());
    }

    #[test]
    fn span_straddling_the_snapshot_is_contiguous() {
        let mut book = OrderBook::new(inst());
        // A batch covering 8..12 while the snapshot is current as of 10 is the
        // documented Binance case: first <= last_applied + 1 <= last.
        book.apply(&delta(
            Sequencing::Span { first: 8, last: 12 },
            &[(Side::Bid, "100.0", "4")],
        ));
        let r = book.apply(&snapshot(
            Sequencing::Span {
                first: 10,
                last: 10,
            },
            &[],
            &[],
        ));
        assert_eq!(r, BookApply::Applied);
        assert_eq!(book.last_sequence(), Some(12));
        assert_eq!(book.best_bid().unwrap().qty, Qty::parse("4").unwrap());
    }

    #[test]
    fn gap_during_buffered_replay_is_reported() {
        let mut book = OrderBook::new(inst());
        book.apply(&delta(Sequencing::Single(11), &[(Side::Bid, "100.0", "1")]));
        book.apply(&delta(Sequencing::Single(20), &[(Side::Bid, "100.0", "2")]));
        // Snapshot at 10 keeps both; 11 applies, 20 is a gap.
        let r = book.apply(&snapshot(Sequencing::Single(10), &[], &[]));
        assert_eq!(
            r,
            BookApply::Gap {
                expected: 12,
                got: 20
            }
        );
        assert_eq!(book.status(), BookStatus::Gapped);
    }

    #[test]
    fn unsequenced_venues_accept_in_arrival_order() {
        let mut book = OrderBook::new(inst());
        book.apply(&snapshot(Sequencing::None, &[("100.0", "1")], &[]));
        let r = book.apply(&delta(Sequencing::None, &[(Side::Bid, "100.5", "1")]));
        assert_eq!(r, BookApply::Applied);
        assert_eq!(book.best_bid().unwrap().price, Px::parse("100.5").unwrap());
        assert_eq!(book.last_sequence(), None);
    }

    #[test]
    fn buffer_overflow_gaps_out_rather_than_growing() {
        let mut book = OrderBook::new(inst());
        for i in 0..MAX_BUFFERED_DELTAS {
            let r = book.apply(&delta(
                Sequencing::Single(i as u64),
                &[(Side::Bid, "100.0", "1")],
            ));
            assert_eq!(r, BookApply::Buffered);
        }
        let r = book.apply(&delta(
            Sequencing::Single(MAX_BUFFERED_DELTAS as u64),
            &[(Side::Bid, "100.0", "1")],
        ));
        assert!(matches!(r, BookApply::Gap { .. }));
        assert_eq!(book.status(), BookStatus::Gapped);
    }

    #[test]
    fn derived_prices() {
        let mut book = OrderBook::new(inst());
        book.apply(&snapshot(
            Sequencing::Single(1),
            &[("100.0", "1")],
            &[("102.0", "3")],
        ));
        assert_eq!(book.mid(), Some(101.0));
        assert_eq!(book.spread(), Some(Px::parse("2.0").unwrap()));
        // Bid qty 1 against ask qty 3 is resting sell pressure, so the fair
        // price sits below the 101.0 mid, nearer the bid.
        let mp = book.microprice().unwrap();
        assert!((mp - 100.5).abs() < 1e-9, "microprice was {mp}");
    }

    #[test]
    fn depth_is_best_first_on_both_sides() {
        let mut book = OrderBook::new(inst());
        book.apply(&snapshot(
            Sequencing::Single(1),
            &[("100.0", "1"), ("101.0", "2"), ("99.0", "3")],
            &[("105.0", "1"), ("103.0", "2"), ("104.0", "3")],
        ));
        let bids: Vec<_> = book
            .depth(Side::Bid, 2)
            .iter()
            .map(|l| l.price.to_string())
            .collect();
        assert_eq!(bids, vec!["101", "100"]);
        let asks: Vec<_> = book
            .depth(Side::Ask, 2)
            .iter()
            .map(|l| l.price.to_string())
            .collect();
        assert_eq!(asks, vec!["103", "104"]);
    }

    #[test]
    fn side_opposite() {
        assert_eq!(Side::Bid.opposite(), Side::Ask);
        assert_eq!(Side::Ask.opposite(), Side::Bid);
    }
}
