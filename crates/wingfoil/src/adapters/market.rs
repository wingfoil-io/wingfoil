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
//! Following the [`statistics`](crate::adapters::statistics) module's pattern, the ops are *not*
//! in the [`prelude`](crate::prelude): bring the extension trait in explicitly
//! with `use wingfoil::adapters::market::MarketBookOps;`.
//!
//! # The five decisions an adapter must not make for itself
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
//! [`BookStatus::Gapped`] and reports [`BookApply::Gap`] carrying a
//! [`GapCause`]. It does *not* keep applying deltas to a book it knows is
//! wrong. The adapter's obligation is to notice and re-request a snapshot;
//! downstream's obligation is to stop trusting the book — which is why
//! [`best_bid`](OrderBook::best_bid) and friends return `None` while gapped,
//! and why the op still ticks on a gap rather than going quiet.
//!
//! Deltas that arrive *after* the gap are [`BookApply::Refused`], which is a
//! different thing from [`BookApply::Stale`] and calls for a different
//! response: stale is routine, refused means the book stays broken until a
//! snapshot arrives. The cause outlives the call on
//! [`OrderBook::gap_cause`], so an adapter reading a book off the graph can
//! still log *which* ids were lost.
//!
//! **4. Deltas that arrive before the snapshot are buffered, not dropped.**
//! Every venue that serves a book as "REST snapshot + WebSocket deltas" has a
//! race: the stream is subscribed first, and its early messages predate the
//! snapshot. [`OrderBook`] buffers them ([`BookApply::Buffered`]), then on
//! snapshot discards the ones the snapshot already covers and replays the
//! rest. Dropping them instead leaves a book that is quietly missing its first
//! few updates.
//!
//! The book only moves *forwards*: a snapshot a live book has already passed is
//! reported [`BookApply::Stale`] and ignored, so a late or duplicate REST
//! response cannot roll the image backwards while still reporting
//! [`BookStatus::Live`]. A gapped book has no baseline to regress from, so its
//! recovery snapshot is always accepted.
//!
//! **5. A burst is applied in full, in order.** Same-instant updates ride one
//! [`Burst`] and every one of them must reach the book — a book is a fold over
//! its whole update history, so collapsing a burst latest-wins silently
//! desynchronises it. Both [`MarketEventOps`] and [`MarketBookOps`] are
//! implemented for the burst shape end to end for this reason; an adapter
//! should never flatten a burst to its last value on the way in.
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
//! On the graph, a venue adapter's event stream becomes a stream of maintained
//! books by demultiplexing with [`MarketEventOps::book_updates`] and wiring
//! [`MarketBookOps::order_book`].
//!
//! Both traits are implemented for the scalar *and* the [`Burst`] shape, and
//! the burst one is what a real adapter holds: `channel`, `external` and
//! `spawn` sources all produce `Stream<Burst<T>>`. The burst travels intact all
//! the way to the book, which is what decision 5 below requires.
//!
//! ```
//! use wingfoil::adapters::market::{
//!     BookSnapshot, BookUpdate, InstrumentId, Level, MarketBookOps, MarketEvent,
//!     MarketEventOps, Px, Qty, Sequencing,
//! };
//! use wingfoil::prelude::*;
//! use wingfoil::{NanoTime, RunFor, RunMode};
//!
//! let inst = InstrumentId::new("example", "BTC-USD");
//! let g = GraphBuilder::new();
//!
//! // What a venue adapter's source looks like from the graph's side: one
//! // multiplexed stream of bursts.
//! let (events, sender) = g.channel::<MarketEvent>();
//! let books = events.book_updates().order_book();
//! let mids = books.map(|b| b.mid()).accumulate();
//! let mut r = g.build();
//!
//! let feed = inst.clone();
//! let producer = std::thread::spawn(move || {
//!     sender.send_at(
//!         MarketEvent::Book(BookUpdate::Snapshot(BookSnapshot {
//!             instrument: feed,
//!             bids: vec![Level::new(Px::parse("100.0").unwrap(), Qty::parse("1").unwrap())],
//!             asks: vec![Level::new(Px::parse("102.0").unwrap(), Qty::parse("1").unwrap())],
//!             sequencing: Sequencing::Single(1),
//!             venue_time: None,
//!             recv_time: NanoTime::ZERO,
//!         })),
//!         NanoTime::new(100),
//!     );
//!     sender.close();
//! });
//!
//! r.run(RunMode::HistoricalFrom(NanoTime::ZERO), RunFor::Forever)?;
//! producer.join().expect("producer thread");
//! assert_eq!(r.value(&mids), vec![Some(101.0)]);
//! # Ok::<(), anyhow::Error>(())
//! ```

use std::collections::BTreeMap;
use std::fmt;
use std::sync::Arc;

use anyhow::{Result, anyhow, bail};

use crate::Burst;
use crate::adapters::common::{Sym, SymbolInterner};
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
pub const SCALE: i128 = 1_000_000_000;

/// Parse a plain decimal string (`-?digits[.digits]`) into a scaled integer.
///
/// Deliberately hand-rolled rather than routed through `f64`: the whole point
/// is to avoid a binary floating point round trip. Rejects exponent notation
/// and any fractional precision that would be lost, so a venue whose tick size
/// is finer than [`DECIMALS`] fails loudly at the adapter boundary instead of
/// silently corrupting book keys.
///
/// # What it accepts
///
/// Surrounding whitespace is trimmed, an explicit leading `+` is allowed, and
/// either side of the point may be empty as long as one digit is present
/// overall — so `" 1.5 "`, `"+0.5"`, `".5"` and `"1."` all parse. Everything
/// else is rejected: exponents (`"1e-9"`), digit separators (`"1_000"`),
/// non-numeric text, and any fractional digit beyond [`DECIMALS`] that is not
/// zero.
fn parse_fixed(s: &str) -> Result<i128> {
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

    let mut value: i128 = 0;
    for b in int_part.bytes() {
        value = value
            .checked_mul(10)
            .and_then(|v| v.checked_add((b - b'0') as i128))
            .ok_or_else(|| anyhow!("decimal string {s:?} overflows the fixed-point range"))?;
    }
    value = value
        .checked_mul(SCALE)
        .ok_or_else(|| anyhow!("decimal string {s:?} overflows the fixed-point range"))?;

    let mut scale = SCALE;
    for (i, b) in frac_part.bytes().enumerate() {
        let digit = (b - b'0') as i128;
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
fn fmt_fixed(raw: i128, f: &mut fmt::Formatter<'_>) -> fmt::Result {
    let neg = raw < 0;
    // `unsigned_abs` rather than `abs` so `i128::MIN` does not panic.
    let mag = raw.unsigned_abs();
    let int = mag / SCALE as u128;
    let frac = mag % SCALE as u128;
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
        /// venue's own decimal text wherever possible;
        /// [`try_from_f64`](Self::try_from_f64) rounds and is a lossy
        /// convenience for tests and display code.
        ///
        /// # Range
        ///
        /// Backed by an `i128`, so the representable range is
        /// `±170_141_183_460_469_231_731.687_303_715_884_105_727` — about
        /// ±1.7 × 10²⁰ at nine decimal places.
        ///
        /// The width is `i128` rather than `i64` because price and quantity
        /// want opposite things from one shared scale: price wants precision at
        /// modest magnitude, quantity wants magnitude at modest precision. Nine
        /// decimals in an `i64` caps both at ±9.22 × 10⁹, and book levels above
        /// ten billion units are routine on meme-coin pairs (SHIB, PEPE,
        /// BONK) — so an `i64` would have made those venues unrepresentable
        /// rather than merely imprecise. The cost is 16 bytes per value and a
        /// two-word compare on the `BTreeMap` key.
        ///
        /// A venue whose values still exceed that, or whose tick size is finer
        /// than [`DECIMALS`], is out of scope for this representation, and
        /// [`parse`](Self::parse) says so rather than truncating.
        #[derive(Clone, Copy, Debug, Default, PartialEq, Eq, PartialOrd, Ord, Hash)]
        pub struct $name(i128);

        impl $name {
            #[doc = concat!("The zero ", $what, ".")]
            pub const ZERO: Self = Self(0);

            /// Wrap a raw scaled integer (already multiplied by [`SCALE`]).
            pub const fn from_raw(raw: i128) -> Self {
                Self(raw)
            }

            /// The underlying scaled integer.
            pub const fn raw(self) -> i128 {
                self.0
            }

            /// Parse the venue's decimal text exactly, with no `f64` round trip.
            pub fn parse(s: &str) -> Result<Self> {
                parse_fixed(s).map(Self)
            }

            /// Round an `f64` into fixed point.
            ///
            /// Lossy by nature — prefer [`parse`](Self::parse) on anything that
            /// came off a wire. Fallible rather than saturating: NaN, the
            /// infinities and values outside the representable range are
            /// errors, because a silently-zeroed NaN or a saturated price is
            /// exactly the corruption the rest of this module exists to
            /// prevent.
            pub fn try_from_f64(v: f64) -> Result<Self> {
                if !v.is_finite() {
                    bail!("cannot convert non-finite f64 {v} into fixed point");
                }
                let scaled = (v * SCALE as f64).round();
                // The bounds are exact powers of two in `f64`, so this compares
                // cleanly; the cast below would saturate rather than wrap, but
                // saturating silently is the behaviour being rejected.
                if scaled < -(2f64.powi(127)) || scaled >= 2f64.powi(127) {
                    bail!("f64 {v} overflows the fixed-point range");
                }
                Ok(Self(scaled as i128))
            }

            /// Convert to `f64`, for arithmetic and the `f64`-typed
            /// [`statistics`](crate::adapters::statistics) ops. Lossy above 2⁵³ raw units.
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
/// Both fields are [`Sym`] — the tree's shared interned-symbol type, from
/// [`adapters::common`](crate::adapters::common) — so the id can ride on every
/// event without a per-tick allocation. An adapter builds one per subscription
/// and clones it into each message; cloning is two atomic increments and no
/// allocation.
///
/// Use [`interned`](Self::interned) with a
/// [`SymbolInterner`](crate::adapters::common::SymbolInterner) held for the
/// life of the connection when building ids for many symbols, so the venue
/// name is allocated once rather than once per instrument.
///
/// The `Default` impl (an empty venue and symbol) exists only because the
/// engine requires every stream's value type to be `Default` for its
/// pre-first-tick value slot. It is not a meaningful instrument.
#[derive(Clone, Debug, Default, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct InstrumentId {
    /// The venue's short name, lowercase by convention (`"binance"`).
    pub venue: Sym,
    /// The venue's own symbol, verbatim and *not* normalised — venues disagree
    /// about separators and casing, and rewriting the symbol loses the ability
    /// to echo it back on a subscribe.
    pub symbol: Sym,
}

impl InstrumentId {
    /// Build an instrument id from a venue name and the venue's own symbol.
    pub fn new(venue: impl AsRef<str>, symbol: impl AsRef<str>) -> Self {
        Self {
            venue: Sym::new(venue),
            symbol: Sym::new(symbol),
        }
    }

    /// Build an instrument id through an interner, sharing storage with every
    /// other id built through the same one.
    pub fn interned(
        interner: &mut SymbolInterner,
        venue: impl AsRef<str>,
        symbol: impl AsRef<str>,
    ) -> Self {
        Self {
            venue: interner.intern(venue.as_ref()),
            symbol: interner.intern(symbol.as_ref()),
        }
    }

    /// Equality with an allocation-sharing fast path.
    ///
    /// Semantically identical to `==`; it just tries `Arc` pointer equality
    /// first, which succeeds whenever both ids descend from the same original
    /// (the overwhelmingly common case — an adapter clones one id into every
    /// message it emits). Falls back to comparing the strings, so ids built
    /// independently still compare equal.
    ///
    /// Used on the per-message path in [`MarketBookOps::order_book`], where the
    /// string compare showed up for no benefit.
    pub fn same_as(&self, other: &InstrumentId) -> bool {
        (self.venue.ptr_eq(&other.venue) && self.symbol.ptr_eq(&other.symbol)) || self == other
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

/// Why a book gave up on the image it was maintaining.
///
/// Carried by [`BookApply::Gap`] and retained on the book itself
/// ([`OrderBook::gap_cause`]) so the ids survive the trip through the graph —
/// the op ticks the book, not the [`BookApply`], and an adapter that wants to
/// log *which* messages were lost reads them back off the book.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum GapCause {
    /// A sequence discontinuity: messages were lost in transit.
    Sequence {
        /// The update id the book expected next.
        expected: u64,
        /// The first update id the message actually carried.
        got: u64,
    },
    /// The pre-snapshot buffer hit [`MAX_BUFFERED_DELTAS`] — the snapshot never
    /// arrived, so no correct book can be built from what was received.
    ///
    /// Distinct from [`Sequence`](Self::Sequence) because no ids are involved:
    /// the venue may not sequence at all, and reporting a fabricated
    /// `expected`/`got` pair (as this once did) says something false about
    /// what happened.
    BufferOverflow {
        /// How many deltas were buffered when the limit was hit.
        buffered: usize,
    },
}

/// What [`OrderBook::apply`] did with an update.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BookApply {
    /// The book advanced.
    Applied,
    /// The update is already covered by the image the book holds, and was
    /// discarded. Normal during the snapshot race; not an error, and no action
    /// is required of the adapter.
    Stale,
    /// The book is [`BookStatus::Gapped`], so the update was refused rather
    /// than applied to an image known to be wrong.
    ///
    /// Distinct from [`Stale`](Self::Stale) because the two call for opposite
    /// responses: stale is routine, refused means the book stays broken until
    /// the adapter re-requests a snapshot.
    Refused,
    /// Buffered pending a snapshot.
    Buffered,
    /// The book has been cleared and is now [`BookStatus::Gapped`]; the adapter
    /// must re-request a snapshot.
    Gap(GapCause),
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
    gap_cause: Option<GapCause>,
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
            gap_cause: None,
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

    /// Why the book last gapped, while it is [`BookStatus::Gapped`].
    ///
    /// Cleared when a snapshot restores the book. This is how the gap details
    /// reach the graph: [`MarketBookOps::order_book`] ticks the book rather
    /// than the [`BookApply`], so an adapter or a monitoring node reads the
    /// cause back off the book it received.
    pub fn gap_cause(&self) -> Option<GapCause> {
        self.gap_cause
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
        // A snapshot the live book has already moved past is a late or
        // duplicate REST response — applying it would roll the book *backwards*
        // to an older image while still reporting `Live`, which is the one way
        // this module could hand downstream a wrong book without saying so.
        //
        // Only a live book can regress: `gap_out` clears `last_seq`, so a
        // recovery snapshot after a gap is always accepted no matter what id it
        // carries. Deliberately `<=` rather than `<` — a snapshot at exactly
        // the id we hold adds nothing and re-clearing would discard the deltas
        // applied on top of it.
        if self.status == BookStatus::Live
            && let (Some(last_applied), Some(snap_last)) = (self.last_seq, s.sequencing.last())
            && snap_last <= last_applied
        {
            return BookApply::Stale;
        }

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
        self.gap_cause = None;
        self.venue_time = s.venue_time;
        self.recv_time = s.recv_time;

        // Replay whatever arrived while we were waiting for this image. Taking
        // the buffer first means a gap discovered mid-replay leaves nothing
        // stale behind for the *next* snapshot to replay a second time.
        let pending = std::mem::take(&mut self.pending);
        for d in &pending {
            match self.apply_delta(d) {
                BookApply::Gap(cause) => return BookApply::Gap(cause),
                _ => continue,
            }
        }
        BookApply::Applied
    }

    fn apply_delta(&mut self, d: &BookDelta) -> BookApply {
        match self.status {
            BookStatus::AwaitingSnapshot => {
                if self.pending.len() >= MAX_BUFFERED_DELTAS {
                    let buffered = self.pending.len();
                    let cause = GapCause::BufferOverflow { buffered };
                    self.gap_out(cause);
                    return BookApply::Gap(cause);
                }
                self.pending.push(d.clone());
                BookApply::Buffered
            }
            // A gapped book refuses deltas outright: applying them would build
            // on an image already known to be wrong.
            BookStatus::Gapped => BookApply::Refused,
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
                        let cause = GapCause::Sequence {
                            expected: last_applied + 1,
                            got: first,
                        };
                        self.gap_out(cause);
                        return BookApply::Gap(cause);
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
    fn gap_out(&mut self, cause: GapCause) {
        self.bids.clear();
        self.asks.clear();
        self.pending.clear();
        self.last_seq = None;
        self.status = BookStatus::Gapped;
        self.gap_cause = Some(cause);
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

    /// Number of distinct price levels on one side. Zero unless the book is
    /// [`BookStatus::Live`], for the same fail-safe reason as
    /// [`best_bid`](Self::best_bid) — today `gap_out` clears both sides so the
    /// gate is redundant, but it stops being redundant the moment gap handling
    /// changes to retain levels, and a caller sizing a buffer off this should
    /// not be the thing that discovers it.
    pub fn level_count(&self, side: Side) -> usize {
        if !self.is_live() {
            return 0;
        }
        match side {
            Side::Bid => self.bids.len(),
            Side::Ask => self.asks.len(),
        }
    }

    /// Mid price, as `f64` for the [`statistics`](crate::adapters::statistics) ops. `None` unless
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
            BookApply::Applied | BookApply::Gap(_) => Tick::Value(Arc::clone(book)),
            // `Refused` does not tick: the book was already gapped, so the
            // tick that carried that news has been sent and nothing changed.
            BookApply::Stale | BookApply::Refused | BookApply::Buffered => Tick::Quiet,
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
                BookApply::Applied | BookApply::Gap(_) => advanced = true,
                BookApply::Stale | BookApply::Refused | BookApply::Buffered => {}
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
///
/// The check runs on every message even though the wiring it catches is fixed
/// after the first, because [`InstrumentId::same_as`] makes it two pointer
/// compares in the case that actually occurs — the adapter clones one id into
/// every message, so both `Arc`s hit. That is cheap enough not to trade the
/// guarantee away, and a `debug_assert` would leave release builds silently
/// interleaving two venues into one book, which is precisely the failure this
/// exists to prevent.
fn book_for<'s>(
    state: &'s mut Option<Arc<OrderBook>>,
    update: &BookUpdate,
) -> Result<&'s mut Arc<OrderBook>> {
    let book = state.get_or_insert_with(|| Arc::new(OrderBook::new(update.instrument().clone())));
    if !book.instrument().same_as(update.instrument()) {
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

// --- demultiplexing -------------------------------------------------------
//
// Four small ops rather than `map_filter`, for two reasons. `map_filter`'s
// signature demands a value in the false branch, so filtering a multiplexed
// venue stream through it would build and discard a `Trade::default()` (two
// `Arc<str>` allocations, via `InstrumentId`) for every message that did *not*
// match — per-message work on the filtered path. And it has no burst-preserving
// form, which the burst impls below need.

/// Selects the [`Trade`]s out of a [`MarketEvent`] stream.
pub struct MarketTradesOp;

#[op(build = market_trades)]
impl Op for MarketTradesOp {
    type Cfg = ();
    type State = ();
    type In<'a> = (&'a MarketEvent,);
    type Out = Trade;
    const ACTIVATION: Activation = Activation::NONE;

    fn cycle(
        _cfg: &mut (),
        _state: &mut (),
        input: (&MarketEvent,),
        _ctx: &mut Ctx<'_>,
    ) -> Result<Tick<Trade>> {
        Ok(match input.0 {
            MarketEvent::Trade(t) => Tick::Value(t.clone()),
            MarketEvent::Book(_) => Tick::Quiet,
        })
    }
}

/// Selects the [`BookUpdate`]s out of a [`MarketEvent`] stream.
pub struct MarketBookUpdatesOp;

#[op(build = market_book_updates)]
impl Op for MarketBookUpdatesOp {
    type Cfg = ();
    type State = ();
    type In<'a> = (&'a MarketEvent,);
    type Out = BookUpdate;
    const ACTIVATION: Activation = Activation::NONE;

    fn cycle(
        _cfg: &mut (),
        _state: &mut (),
        input: (&MarketEvent,),
        _ctx: &mut Ctx<'_>,
    ) -> Result<Tick<BookUpdate>> {
        Ok(match input.0 {
            MarketEvent::Book(b) => Tick::Value(b.clone()),
            MarketEvent::Trade(_) => Tick::Quiet,
        })
    }
}

/// Selects the [`Trade`]s out of a burst of [`MarketEvent`]s, preserving the
/// burst.
///
/// Quiet when the burst held no trades, so a book-only burst does not tick an
/// empty group downstream.
pub struct MarketTradesBurstOp;

#[op(build = market_trades_bursts)]
impl Op for MarketTradesBurstOp {
    type Cfg = ();
    type State = ();
    type In<'a> = (&'a Burst<MarketEvent>,);
    type Out = Burst<Trade>;
    const ACTIVATION: Activation = Activation::NONE;

    fn cycle(
        _cfg: &mut (),
        _state: &mut (),
        input: (&Burst<MarketEvent>,),
        _ctx: &mut Ctx<'_>,
    ) -> Result<Tick<Burst<Trade>>> {
        let mut out = Burst::new();
        for e in input.0.iter() {
            if let MarketEvent::Trade(t) = e {
                out.push(t.clone());
            }
        }
        Ok(if out.is_empty() {
            Tick::Quiet
        } else {
            Tick::Value(out)
        })
    }
}

/// Selects the [`BookUpdate`]s out of a burst of [`MarketEvent`]s, preserving
/// the burst.
///
/// Preserving it is the whole point: the group must reach
/// [`order_book`](MarketBookOps::order_book) intact, because a book has to
/// apply *every* update in arrival order. Quiet when the burst held no book
/// messages.
pub struct MarketBookUpdatesBurstOp;

#[op(build = market_book_updates_bursts)]
impl Op for MarketBookUpdatesBurstOp {
    type Cfg = ();
    type State = ();
    type In<'a> = (&'a Burst<MarketEvent>,);
    type Out = Burst<BookUpdate>;
    const ACTIVATION: Activation = Activation::NONE;

    fn cycle(
        _cfg: &mut (),
        _state: &mut (),
        input: (&Burst<MarketEvent>,),
        _ctx: &mut Ctx<'_>,
    ) -> Result<Tick<Burst<BookUpdate>>> {
        let mut out = Burst::new();
        for e in input.0.iter() {
            if let MarketEvent::Book(b) = e {
                out.push(b.clone());
            }
        }
        Ok(if out.is_empty() {
            Tick::Quiet
        } else {
            Tick::Value(out)
        })
    }
}

/// Demultiplexing a combined [`MarketEvent`] stream into its parts.
///
/// Implemented for both `Stream<MarketEvent>` and `Stream<Burst<MarketEvent>>`,
/// preserving the shape: the burst form is the one a real adapter holds, since
/// `channel` / `external` / `spawn` sources all produce
/// [`Burst`]es. [`MarketBookOps`] covers both shapes too, so
/// `events.book_updates().order_book()` wires either way.
///
/// Not in the [`prelude`](crate::prelude) — `use
/// wingfoil::adapters::market::MarketEventOps;`.
pub trait MarketEventOps {
    /// The trade stream this event stream demultiplexes into — `Stream<Trade>`
    /// for a scalar stream, `Stream<Burst<Trade>>` for a burst stream.
    type Trades;
    /// The book-message stream, likewise shape-preserving.
    type Books;

    /// Just the trade prints.
    fn trades(&self) -> Self::Trades;
    /// Just the book messages — wire this into
    /// [`order_book`](MarketBookOps::order_book).
    fn book_updates(&self) -> Self::Books;
}

impl MarketEventOps for Stream<MarketEvent> {
    type Trades = Stream<Trade>;
    type Books = Stream<BookUpdate>;

    fn trades(&self) -> Stream<Trade> {
        self.wire(|b, h| b.market_trades(h))
    }

    fn book_updates(&self) -> Stream<BookUpdate> {
        self.wire(|b, h| b.market_book_updates(h))
    }
}

impl MarketEventOps for Stream<Burst<MarketEvent>> {
    type Trades = Stream<Burst<Trade>>;
    type Books = Stream<Burst<BookUpdate>>;

    fn trades(&self) -> Stream<Burst<Trade>> {
        self.wire(|b, h| b.market_trades_bursts(h))
    }

    fn book_updates(&self) -> Stream<Burst<BookUpdate>> {
        self.wire(|b, h| b.market_book_updates_bursts(h))
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
        // Wider than the i128 backing store: 40 digits scaled by 1e9.
        assert!(Px::parse("9999999999999999999999999999999999999999").is_err());
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
            BookApply::Gap(GapCause::Sequence {
                expected: 11,
                got: 13
            })
        );
        assert_eq!(book.status(), BookStatus::Gapped);

        // Fails safe: the touch reads empty even though levels were once known.
        assert_eq!(book.best_bid(), None);
        assert_eq!(book.best_ask(), None);
        assert_eq!(book.mid(), None);
        assert!(book.depth(Side::Bid, 5).is_empty());

        // Further deltas are refused rather than applied to a wrong image —
        // `Refused`, not `Stale`: the adapter must re-request a snapshot.
        let r = book.apply(&delta(Sequencing::Single(14), &[(Side::Bid, "100.5", "1")]));
        assert_eq!(r, BookApply::Refused);
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
            BookApply::Gap(GapCause::Sequence {
                expected: 12,
                got: 20
            })
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
        assert_eq!(
            r,
            BookApply::Gap(GapCause::BufferOverflow {
                buffered: MAX_BUFFERED_DELTAS
            })
        );
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

    // --- range and f64 conversion ----------------------------------------

    #[test]
    fn quantities_beyond_the_i64_ceiling_are_representable() {
        // The i64 backing this once had capped both types at ±9.22e9, which
        // made meme-coin book levels unrepresentable rather than imprecise.
        let q = Qty::parse("10000000000").unwrap();
        assert_eq!(q.to_string(), "10000000000");
        // A SHIB-scale resting quantity, with fractional precision intact.
        let q = Qty::parse("123456789012345.678901234").unwrap();
        assert_eq!(q.to_string(), "123456789012345.678901234");
        // Ordering still holds out at that magnitude.
        assert!(Qty::parse("10000000000").unwrap() < Qty::parse("10000000001").unwrap());
    }

    #[test]
    fn try_from_f64_rejects_what_it_cannot_represent() {
        // The whole point: no silent NaN-to-zero, no saturation.
        assert!(Px::try_from_f64(f64::NAN).is_err());
        assert!(Px::try_from_f64(f64::INFINITY).is_err());
        assert!(Px::try_from_f64(f64::NEG_INFINITY).is_err());
        assert!(Px::try_from_f64(1e30).is_err());
        assert_eq!(Px::try_from_f64(1.5).unwrap(), Px::parse("1.5").unwrap());
        assert_eq!(
            Px::try_from_f64(-2.25).unwrap(),
            Px::parse("-2.25").unwrap()
        );
    }

    // --- instrument ids ---------------------------------------------------

    #[test]
    fn same_as_matches_across_independently_built_ids() {
        let a = InstrumentId::new("test", "BTC-USD");
        let b = a.clone();
        // The common case: cloned from one original, so the pointers hit.
        assert!(a.same_as(&b));
        // Built independently — pointers miss, content compare must still say
        // equal, or the fast path would be a correctness bug.
        let c = InstrumentId::new("test", "BTC-USD");
        assert!(a.same_as(&c));
        assert!(!a.same_as(&InstrumentId::new("test", "ETH-USD")));
        assert!(!a.same_as(&InstrumentId::new("other", "BTC-USD")));
    }

    #[test]
    fn interned_ids_share_storage() {
        let mut interner = SymbolInterner::default();
        let a = InstrumentId::interned(&mut interner, "binance", "BTCUSDT");
        let b = InstrumentId::interned(&mut interner, "binance", "ETHUSDT");
        // One allocation for the venue across every instrument on it.
        assert!(a.venue.ptr_eq(&b.venue));
        assert!(!a.symbol.ptr_eq(&b.symbol));
        assert_eq!(a.to_string(), "binance:BTCUSDT");
    }

    // --- the contracts the review surfaced --------------------------------

    #[test]
    fn a_stale_snapshot_does_not_rewind_a_live_book() {
        let mut book = OrderBook::new(inst());
        book.apply(&snapshot(
            Sequencing::Single(10),
            &[("100.0", "1")],
            &[("101.0", "1")],
        ));
        book.apply(&delta(Sequencing::Single(11), &[(Side::Bid, "100.5", "5")]));

        // A late or duplicate REST response, current as of an older id. It must
        // not roll the book back to the older image.
        let r = book.apply(&snapshot(Sequencing::Single(9), &[("1.0", "1")], &[]));
        assert_eq!(r, BookApply::Stale);
        assert_eq!(book.status(), BookStatus::Live);
        assert_eq!(book.last_sequence(), Some(11));
        assert_eq!(book.best_bid().unwrap().price, Px::parse("100.5").unwrap());

        // A snapshot at exactly the id we hold likewise changes nothing.
        let r = book.apply(&snapshot(Sequencing::Single(11), &[("1.0", "1")], &[]));
        assert_eq!(r, BookApply::Stale);
        assert_eq!(book.best_bid().unwrap().price, Px::parse("100.5").unwrap());

        // But a newer one is applied.
        let r = book.apply(&snapshot(Sequencing::Single(12), &[("200.0", "1")], &[]));
        assert_eq!(r, BookApply::Applied);
        assert_eq!(book.best_bid().unwrap().price, Px::parse("200.0").unwrap());
    }

    #[test]
    fn a_gapped_book_accepts_a_recovery_snapshot_at_any_id() {
        let mut book = OrderBook::new(inst());
        book.apply(&snapshot(Sequencing::Single(100), &[("100.0", "1")], &[]));
        book.apply(&delta(
            Sequencing::Single(200),
            &[(Side::Bid, "100.0", "1")],
        ));
        assert_eq!(book.status(), BookStatus::Gapped);

        // Lower than the 100 the book had reached before it gapped: the
        // regression guard must not fire here, or a gapped book could never
        // recover from a venue that resets its ids.
        let r = book.apply(&snapshot(Sequencing::Single(5), &[("50.0", "1")], &[]));
        assert_eq!(r, BookApply::Applied);
        assert_eq!(book.status(), BookStatus::Live);
        assert_eq!(book.last_sequence(), Some(5));
    }

    #[test]
    fn gap_cause_survives_for_the_adapter_to_read() {
        let mut book = OrderBook::new(inst());
        assert_eq!(book.gap_cause(), None);
        book.apply(&snapshot(Sequencing::Single(10), &[("100.0", "1")], &[]));

        book.apply(&delta(Sequencing::Single(13), &[(Side::Bid, "100.0", "1")]));
        // The ids reach the graph through the book, not the `BookApply`.
        assert_eq!(
            book.gap_cause(),
            Some(GapCause::Sequence {
                expected: 11,
                got: 13
            })
        );

        // Recovery clears it.
        book.apply(&snapshot(Sequencing::Single(20), &[("100.0", "1")], &[]));
        assert_eq!(book.gap_cause(), None);
    }

    #[test]
    fn overflow_reports_overflow_rather_than_a_fabricated_gap() {
        // An unsequenced venue: there is no id pair to report, and the previous
        // `Gap { expected: 0, got: 0 }` said something false about what
        // happened.
        let mut book = OrderBook::new(inst());
        for _ in 0..MAX_BUFFERED_DELTAS {
            book.apply(&delta(Sequencing::None, &[(Side::Bid, "100.0", "1")]));
        }
        let r = book.apply(&delta(Sequencing::None, &[(Side::Bid, "100.0", "1")]));
        assert_eq!(
            r,
            BookApply::Gap(GapCause::BufferOverflow {
                buffered: MAX_BUFFERED_DELTAS
            })
        );
        assert_eq!(
            book.gap_cause(),
            Some(GapCause::BufferOverflow {
                buffered: MAX_BUFFERED_DELTAS
            })
        );
    }

    #[test]
    fn level_count_is_gated_on_live_like_the_other_accessors() {
        let mut book = OrderBook::new(inst());
        book.apply(&snapshot(
            Sequencing::Single(1),
            &[("100.0", "1")],
            &[("101.0", "1")],
        ));
        assert_eq!(book.level_count(Side::Bid), 1);
        book.apply(&delta(Sequencing::Single(9), &[(Side::Bid, "100.0", "1")]));
        assert_eq!(book.status(), BookStatus::Gapped);
        assert_eq!(book.level_count(Side::Bid), 0);
        assert_eq!(book.level_count(Side::Ask), 0);
    }
}
