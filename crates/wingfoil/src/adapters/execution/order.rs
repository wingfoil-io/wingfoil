//! `Order` / `Fill`: the execution vocabulary.
//!
//! Designed from the venue side, per Project Venue §3.2: what a NewOrderSingle
//! / ExecutionReport (or a venue's own place / order-update messages) actually
//! needs. Prices and quantities are `Px`/`Qty`, never `f64`. Both timestamps
//! are recorded; `recv_time` is engine time from `Ctx::time`, never the wall
//! clock, so a recorded session replays.
//!
//! An order's *state* is not here: that is the OMS's, which is the only
//! thing that sees the reports that move it.
//!
//! # The instrument is a type parameter
//!
//! [`Order`] and [`Fill`] name what they trade as an `I` of the caller's
//! choosing — an options spec, an equity ticker id, a futures contract. It
//! must be `Copy` to keep the vocabulary a plain value (see below), and
//! `Default` for the `Burst` placeholder; nothing here reads it.
//!
//! # The default order is not a sendable order
//!
//! [`Order`] and [`Fill`] have a [`Default`] because a value on a graph edge
//! must have one — a `Burst<T>` is a `TinyVec<[T; 1]>`, which requires it —
//! and not because a default order means anything.
//!
//! A defaulted order is deliberately inert: zero quantity, no limit price, and
//! [`OrderKind::PostOnly`], which is the one kind that cannot cross. Every
//! venue rejects a zero-quantity order, so a default that escaped into the
//! OMS by mistake is refused at the boundary rather than traded. That is the
//! property the tests below pin, and it is why the kind defaults to the
//! passive variant rather than to `Limit` (first-listed) or `Market` (which
//! would cross at any price).
//!
//! # What the field shapes cannot say
//!
//! [`Order`]'s `kind`, `price` and `tif` are separate fields, so combinations
//! no venue accepts are constructible: a market order carrying a limit, a
//! resting order with no level, post-only with a fill-or-kill. They stay
//! separate — an `Option<Px>` is what the wire has and what `Copy` wants.
//!
//! What builds an order is therefore a constructor — [`Order::limit`],
//! [`Order::post_only`], [`Order::market`] — each of which can only produce an
//! agreeing set, and which takes the price for the two kinds that need one and
//! refuses to take it for the one that does not. [`Order::validate`] stays as
//! the backstop for an order assembled some other way (a struct literal, a
//! field edited on a copy), checked once on the way out rather than in each of
//! the OMS, the venue codec and the sim.

use std::fmt;
use std::fmt::Debug;
use std::hash::Hash;

use crate::NanoTime;
use crate::adapters::execution::exec_id::ExecId;
use crate::adapters::market::{Px, Qty, Side};

/// What the stateful parts — the OMS, a position fold — need of an
/// instrument to key on it: a plain value with an order, so a walk over a
/// map of them is stated rather than hashed. A blanket alias, never
/// implemented by hand: any such type is one.
pub trait Instrument: Copy + Eq + Hash + Ord + Default + Debug {}

impl<T: Copy + Eq + Hash + Ord + Default + Debug> Instrument for T {}

/// Client-assigned order identity. Distinct ids keep two otherwise-identical
/// orders distinct values, which is what defuses `TimeQueue` dedup on a
/// feedback edge (Project Venue §5.2).
///
/// # Layout: an epoch above a sequence
///
/// The low 32 bits are the OMS's per-request counter; the 18 above them are
/// the process's [`Epoch`]. A bare counter would restart at
/// the numbers the previous process's orders were labelled with, and a
/// notification about one of those — a cancel the reconnect's `cancel_all`
/// caused, arriving on the new session — would read as news about a
/// different order that happens to hold the same number now. With the epoch
/// in the high bits it names an order of *another* process, which
/// order reconciliation already knows how to treat.
///
/// Eighteen bits, not thirty-two, because the id may ride in a JSON-RPC id
/// that a venue reads as a double: `Epoch::MAX << 32 | u32::MAX` is under
/// 2⁵¹, so it survives that exactly.
/// Epoch 0 is a process that persists nothing — a backtest, a test — whose
/// ids are exactly the bare counter they always were.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct ClientOrderId(pub u64);

impl ClientOrderId {
    /// Sequence `sequence` of epoch `epoch`.
    pub const fn new(epoch: Epoch, sequence: u32) -> ClientOrderId {
        ClientOrderId(((epoch.0 as u64) << 32) | sequence as u64)
    }

    /// The epoch of the process that minted it. An id wider than the layout
    /// — one no OMS here minted — reads as the epoch its bits say, masked.
    pub const fn epoch(self) -> Epoch {
        Epoch(((self.0 >> 32) as u32) & Epoch::MAX.0)
    }

    /// Its place in its epoch's sequence.
    pub const fn sequence(self) -> u32 {
        self.0 as u32
    }

    /// The top bit of the sequence: the half of every epoch's ids the OMS
    /// never mints, reserved for another minter in the same process whose
    /// executions come back on the order edge's reports under its own ids —
    /// a quote fold above the order edge (a request-for-quote responder, say). The OMS counts
    /// below it and passes over a report naming an id above it.
    pub const RESERVED: u32 = 1 << 31;

    /// Whether this id is in the [reserved](Self::RESERVED) half: minted by
    /// something other than the OMS.
    pub const fn is_reserved(self) -> bool {
        self.sequence() & Self::RESERVED != 0
    }
}

/// Which process minted a [`ClientOrderId`].
///
/// **An epoch has to differ from the one before it, not from every one
/// ever.** What it protects against is the previous process's orders — the
/// notifications about them still in flight when this one connects, and an
/// order that outlived its `cancel_all` — so a counter persisted beside the
/// rest of the book's state and stepped on every start is enough, and it
/// wraps rather than running out: an id half a million restarts old names
/// nothing anybody is still holding.
///
/// Where the persisted one is lost, [`from_clock`](Self::from_clock) derives
/// one from the wall clock, which differs from whatever the lost file held
/// with overwhelming probability — the process's identity is not graph data,
/// so that is the one clock that may choose it (the binary does, never the
/// graph).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct Epoch(u32);

impl Epoch {
    /// The epoch of a process that persists nothing: ids are the bare
    /// counter. Never stepped into.
    pub const ZERO: Epoch = Epoch(0);

    /// The largest epoch an id can carry — see [`ClientOrderId`]'s layout.
    pub const MAX: Epoch = Epoch((1 << 18) - 1);

    /// `epoch`, or `None` above [`MAX`](Self::MAX) — refused rather than
    /// masked into a different process's number.
    pub const fn new(epoch: u32) -> Option<Epoch> {
        if epoch > Self::MAX.0 {
            return None;
        }
        Some(Epoch(epoch))
    }

    /// The number.
    pub const fn get(self) -> u32 {
        self.0
    }

    /// The epoch after this one: the next process's. Wraps past
    /// [`MAX`](Self::MAX) to 1, never to [`ZERO`](Self::ZERO).
    pub const fn next(self) -> Epoch {
        if self.0 >= Self::MAX.0 {
            Epoch(1)
        } else {
            Epoch(self.0 + 1)
        }
    }

    /// An epoch for a process with nothing persisted to step from, out of
    /// `unix_secs`. Never [`ZERO`](Self::ZERO).
    pub const fn from_clock(unix_secs: u64) -> Epoch {
        Epoch((unix_secs % Self::MAX.0 as u64) as u32 + 1)
    }
}

/// How the order rests or crosses.
///
/// Defaults to [`PostOnly`](Self::PostOnly): of the three it is the only one
/// that cannot cross, so it is the safe thing for a defaulted value to be.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub enum OrderKind {
    /// Rests at `price`; may cross.
    Limit,
    /// Rests at `price`; rejected (or amended, per venue) if it would cross.
    #[default]
    PostOnly,
    /// Crosses at any price. Prefer a capped immediate-or-cancel limit where
    /// the worst price matters: in a gap a market order's is unbounded.
    Market,
}

/// How long the order lives.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub enum TimeInForce {
    /// Until cancelled.
    #[default]
    GoodTillCancel,
    /// Fill what crosses now, cancel the rest.
    ImmediateOrCancel,
    /// Fill entirely now or not at all.
    FillOrKill,
    /// Until the venue's close, which retires it as expired. A session
    /// venue's resting order.
    Day,
}

/// Which way a [`Trigger`] fires, relative to the order's side.
///
/// Both rest untriggered at the venue until the reference reaches the
/// trigger price; they differ in which way it has to move. The venue is
/// told which one it is rather than left to infer it from where the
/// reference stands, because the reference moves between the decision and
/// the arrival, and an order whose meaning flipped on the way would fire on
/// the wrong side of the market.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub enum TriggerKind {
    /// A stop: a buy fires when the reference rises to the price, a sell when
    /// it falls to it — the order that cuts a position the market is moving
    /// against. FIX `OrdType` 3 or 4, with `StopPx`.
    #[default]
    Stop,
    /// A take: a buy fires when the reference falls to the price, a sell when
    /// it rises to it — the order that takes a profit. FIX's if-touched
    /// orders, `OrdType` J or K.
    Take,
}

/// Which price a [`Trigger`] watches.
///
/// A venue's choice of reference is part of the order, not of the venue:
/// one that fires on the last trade can be set off by a single print in a
/// thin book, and one that fires on an index cannot be set off by the book
/// at all.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub enum Reference {
    /// The venue's mark price: what it values positions at. The default,
    /// because it is the one a single trade cannot move.
    #[default]
    Mark,
    /// The underlying's index.
    Index,
    /// The last trade on the instrument itself.
    Last,
}

/// What makes an order rest untriggered at the venue: it becomes the order it
/// otherwise says — a limit at its price, or a market order where it has none
/// — once [`reference`](Self::reference) reaches [`price`](Self::price) in
/// the direction [`kind`](Self::kind) says.
///
/// A plain value, so an [`Order`] carrying one stays `Copy`.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub struct Trigger {
    /// Stop or take.
    pub kind: TriggerKind,
    /// The price watched.
    pub reference: Reference,
    /// The level the reference has to reach.
    pub price: Px,
}

impl Trigger {
    /// A stop at `price` on `reference`.
    pub const fn stop(reference: Reference, price: Px) -> Trigger {
        Trigger {
            kind: TriggerKind::Stop,
            reference,
            price,
        }
    }

    /// A take at `price` on `reference`.
    pub const fn take(reference: Reference, price: Px) -> Trigger {
        Trigger {
            kind: TriggerKind::Take,
            reference,
            price,
        }
    }

    /// Whether `reference` standing at `at` has reached this trigger, for an
    /// order on `side`.
    ///
    /// The one statement of [`TriggerKind`]'s directions, so a simulator and
    /// a test fire the same orders a reader of the docs expects: a buy stop
    /// fires at or above its price, a sell stop at or below it, and a take
    /// the other way round.
    pub fn fires(&self, side: Side, at: Px) -> bool {
        match (self.kind, side) {
            (TriggerKind::Stop, Side::Bid) | (TriggerKind::Take, Side::Ask) => at >= self.price,
            (TriggerKind::Stop, Side::Ask) | (TriggerKind::Take, Side::Bid) => at <= self.price,
        }
    }
}

/// Which side of the book an execution took — whether it added liquidity or
/// removed it.
///
/// On the [`Fill`] because the fee is a function of it and of nothing else the
/// fill carries: venues charge maker and taker differently. A fee attributed
/// without knowing which side it was is a fee a simulator cannot reproduce
/// and PnL attribution cannot split.
///
/// Defaults to [`Maker`](Self::Maker), which is inert in the same sense as the
/// rest of the defaults here: a defaulted order is post-only, so maker is
/// what an execution against one could only have been.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub enum Liquidity {
    /// The resting side: this execution added liquidity.
    #[default]
    Maker,
    /// The crossing side: this execution removed liquidity.
    Taker,
}

/// An order intent from the strategy to the venue (live) or the sim
/// (backtest). Travels as `Burst<Order>`.
///
/// `Copy`, and deliberately so: orders ride a feedback edge, where a value
/// that allocates is cloned on every hop and on every `TimeQueue` comparison.
/// The venue's spelling of the instrument is rendered at the codec, which is
/// the one place that needs it and is already paying for a socket write.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Order<I> {
    /// Client order id.
    pub id: ClientOrderId,
    /// Which contract to trade — the whole identity, as a value.
    pub instrument: I,
    /// Buy or sell.
    pub side: Side,
    /// Quantity, in the venue's contract unit.
    pub qty: Qty,
    /// Limit price; `None` for a market order.
    pub price: Option<Px>,
    /// Limit / post-only / market.
    pub kind: OrderKind,
    /// Lifetime.
    pub tif: TimeInForce,
    /// Only reduce an existing position.
    pub reduce_only: bool,
    /// Rest untriggered until this fires, then become the order the rest of
    /// the fields say; `None` for an order that is live on arrival.
    pub trigger: Option<Trigger>,
    /// Engine time the strategy emitted it.
    pub created: NanoTime,
}

/// An execution against an [`Order`]. Fills are intrinsically burst-shaped —
/// one order crossing several levels yields several — so they travel as
/// `Burst<Fill>` even for a scalar order in.
///
/// `Copy`, like [`Order`]. The venue's execution id is arbitrary text of the
/// venue's choosing and has to survive verbatim — it is what reconciliation
/// matches on — but [`ExecId`] holds it inline, so carrying it costs the type
/// nothing.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Fill<I> {
    /// The order this executes.
    pub order: ClientOrderId,
    /// Venue-assigned execution id, verbatim.
    pub exec_id: ExecId,
    /// Which contract executed.
    pub instrument: I,
    /// Side of the original order.
    pub side: Side,
    /// Quantity filled in this execution.
    pub qty: Qty,
    /// Everything filled on the order so far, this execution included —
    /// FIX `CumQty` (tag 14).
    ///
    /// With [`remaining`](Self::remaining) (`LeavesQty`, 151) it says the
    /// order's current total, which is what a codec rendering a replace
    /// needs: a FIX `OrderQty` is the total, not what shows (see
    /// [`fix::ReplaceChain`](crate::adapters::execution::fix::ReplaceChain)). Carried rather than
    /// summed by each reader because it is the venue's number, and a reader
    /// that missed one execution — a restart, a gap — would count short for
    /// the rest of the order's life. It is not `qty` minus anything: an
    /// amend changes what shows and leaves what has filled alone.
    ///
    /// A settlement, a delivery or a re-base is one execution for its whole
    /// size, so there it is [`qty`](Self::qty).
    pub filled: Qty,
    /// Remaining open quantity on the order after this execution — FIX
    /// `LeavesQty` (tag 151).
    pub remaining: Qty,
    /// Execution price.
    pub price: Px,
    /// Fee charged on this execution, as an *amount* — not a price.
    ///
    /// `Qty` rather than `Px` because a fee is a quantity of currency: it is
    /// added to a cash balance, never compared with a price or a strike, and
    /// typing it as a price invites exactly that arithmetic. The currency is
    /// the instrument's settlement currency and is not carried as a field: a
    /// separate field could only ever disagree with the instrument.
    ///
    /// Negative is a rebate.
    pub fee: Qty,
    /// Whether this execution made or took liquidity — what the fee schedule
    /// is keyed on.
    pub liquidity: Liquidity,
    /// The venue's own timestamp for the execution, if it sends one.
    pub venue_time: Option<NanoTime>,
    /// Engine time the fill was received (`Ctx::time`, never `NanoTime::now`).
    pub recv_time: NanoTime,
}

/// Why an [`Order`] is not sendable.
///
/// One variant per way [`kind`](Order::kind), [`price`](Order::price) and
/// [`tif`](Order::tif) can contradict each other, plus the quantity. The
/// reasoning behind each is on the variant, not in the message: a log line
/// wants the fact, and the reader of the code wants the argument.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum OrderError {
    /// Zero or negative quantity.
    ///
    /// Every venue refuses it, and it is what a defaulted [`Order`] carries —
    /// which is the property that makes the [`Default`] safe to have. Negative
    /// is not a short: [`Order::side`] says that.
    NonPositiveQuantity(Qty),
    /// [`OrderKind::Market`] with a price.
    ///
    /// A market order crosses at any price, so the limit would be silently
    /// ignored — a lie about where the order will trade.
    MarketWithPrice(Px),
    /// [`OrderKind::Limit`] or [`OrderKind::PostOnly`] with no price.
    ///
    /// A resting order has to name the level it rests at.
    RestingWithoutPrice(OrderKind),
    /// [`OrderKind::PostOnly`] with [`TimeInForce::ImmediateOrCancel`] or
    /// [`TimeInForce::FillOrKill`].
    ///
    /// Post-only cannot cross and those two only fill by crossing, so the pair
    /// can only ever cancel.
    PostOnlyCannotCross(TimeInForce),
    /// A [`Trigger`] on a post-only order.
    ///
    /// A triggered order exists to trade once the reference gets there, and
    /// a post-only one is refused exactly when it would: the pair can only
    /// rest at a level the market has already left, or be refused on the
    /// instant it fires.
    TriggeredPostOnly,
    /// A [`Trigger`] on an order that is [`TimeInForce::ImmediateOrCancel`]
    /// or [`TimeInForce::FillOrKill`].
    ///
    /// Those lifetimes end on arrival, and a triggered order has to rest
    /// until it fires. What happens to the order the trigger *makes* is the
    /// venue's rule, and stating it here would be a promise the venue does
    /// not keep everywhere.
    TriggeredImmediate(TimeInForce),
}

impl fmt::Display for OrderError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::NonPositiveQuantity(qty) => write!(f, "quantity {qty} is not positive"),
            Self::MarketWithPrice(px) => write!(f, "a market order carries a price of {px}"),
            Self::RestingWithoutPrice(kind) => write!(f, "a {kind:?} order has no price"),
            Self::PostOnlyCannotCross(tif) => write!(f, "a post-only order is {tif:?}"),
            Self::TriggeredPostOnly => f.write_str("a triggered order is post-only"),
            Self::TriggeredImmediate(tif) => write!(f, "a triggered order is {tif:?}"),
        }
    }
}

impl std::error::Error for OrderError {}

impl<I> Order<I> {
    /// Every field a constructor sets; the rest are the inert defaults.
    const fn with(
        id: ClientOrderId,
        instrument: I,
        side: Side,
        qty: Qty,
        price: Option<Px>,
        kind: OrderKind,
        tif: TimeInForce,
    ) -> Order<I> {
        Order {
            id,
            instrument,
            side,
            qty,
            price,
            kind,
            tif,
            reduce_only: false,
            trigger: None,
            created: NanoTime::ZERO,
        }
    }

    /// A limit order resting at `price`, good till cancelled.
    ///
    /// May cross on arrival — that is what separates it from
    /// [`post_only`](Self::post_only). The fields this leaves at their
    /// defaults are set by struct update: `Order { created, reduce_only: true,
    /// ..Order::limit(..) }`.
    pub const fn limit(
        id: ClientOrderId,
        instrument: I,
        side: Side,
        qty: Qty,
        price: Px,
    ) -> Order<I> {
        Order::with(
            id,
            instrument,
            side,
            qty,
            Some(price),
            OrderKind::Limit,
            TimeInForce::GoodTillCancel,
        )
    }

    /// A post-only order resting at `price`, good till cancelled.
    ///
    /// Good-till-cancel is not a default to be overridden here: post-only with
    /// an immediate-or-cancel or fill-or-kill lifetime can only ever cancel,
    /// which is [`OrderError::PostOnlyCannotCross`].
    pub const fn post_only(
        id: ClientOrderId,
        instrument: I,
        side: Side,
        qty: Qty,
        price: Px,
    ) -> Order<I> {
        Order::with(
            id,
            instrument,
            side,
            qty,
            Some(price),
            OrderKind::PostOnly,
            TimeInForce::GoodTillCancel,
        )
    }

    /// A market order for `qty`.
    ///
    /// No price, because a market order crosses at any price and a limit on
    /// one would be silently ignored. Immediate-or-cancel, because a market
    /// order that did not fill now has nothing left to be: what does not cross
    /// is not going to become a level to rest at.
    pub const fn market(id: ClientOrderId, instrument: I, side: Side, qty: Qty) -> Order<I> {
        Order::with(
            id,
            instrument,
            side,
            qty,
            None,
            OrderKind::Market,
            TimeInForce::ImmediateOrCancel,
        )
    }

    /// Whether this order says one thing.
    ///
    /// The constructors above cannot build a disagreeing order, so this is the
    /// backstop for one assembled another way — a struct literal, a field
    /// edited on a copy, a decoded order off a venue's own reply.
    ///
    /// Call it on the way out: the OMS rejects an order that fails this before
    /// it reaches a socket, and the sim before it reaches a book. A venue would
    /// reject it too, but a venue rejection arrives asynchronously, after the
    /// quoting loop has already counted the order as working.
    ///
    /// # Errors
    ///
    /// One [`OrderError`] per way the fields can contradict each other; each
    /// variant carries the argument for itself.
    pub fn validate(&self) -> Result<(), OrderError> {
        if self.qty <= Qty::ZERO {
            return Err(OrderError::NonPositiveQuantity(self.qty));
        }
        match (self.kind, self.price) {
            (OrderKind::Market, Some(price)) => return Err(OrderError::MarketWithPrice(price)),
            (kind @ (OrderKind::Limit | OrderKind::PostOnly), None) => {
                return Err(OrderError::RestingWithoutPrice(kind));
            }
            _ => {}
        }
        if self.kind == OrderKind::PostOnly
            && matches!(
                self.tif,
                TimeInForce::ImmediateOrCancel | TimeInForce::FillOrKill
            )
        {
            return Err(OrderError::PostOnlyCannotCross(self.tif));
        }
        if self.trigger.is_some() {
            if self.kind == OrderKind::PostOnly {
                return Err(OrderError::TriggeredPostOnly);
            }
            if matches!(
                self.tif,
                TimeInForce::ImmediateOrCancel | TimeInForce::FillOrKill
            ) {
                return Err(OrderError::TriggeredImmediate(self.tif));
            }
        }
        Ok(())
    }

    /// [`validate`](Self::validate) as a predicate, for a filter on the order
    /// edge that drops rather than reports.
    pub fn is_well_formed(&self) -> bool {
        self.validate().is_ok()
    }
}

/// See the module docs: inert, not meaningful. `Side` has no `Default` in
/// wingfoil's `market` vocabulary — deliberately, since neither side is the
/// obvious one — so this is written out rather than derived, and `Bid` is a
/// placeholder that the zero quantity makes unreachable in practice.
impl<I: Default> Default for Order<I> {
    fn default() -> Self {
        Order {
            id: ClientOrderId::default(),
            instrument: I::default(),
            side: Side::Bid,
            qty: Qty::default(),
            price: None,
            kind: OrderKind::default(),
            tif: TimeInForce::default(),
            reduce_only: false,
            trigger: None,
            created: NanoTime::default(),
        }
    }
}

/// As [`Order`]: a placeholder so a fill can ride a `Burst`, with a zero
/// quantity and an empty [`ExecId`] to say it is not an execution any venue
/// reported.
impl<I: Default> Default for Fill<I> {
    fn default() -> Self {
        Fill {
            order: ClientOrderId::default(),
            exec_id: ExecId::default(),
            instrument: I::default(),
            side: Side::Bid,
            qty: Qty::default(),
            filled: Qty::ZERO,
            remaining: Qty::default(),
            price: Px::default(),
            fee: Qty::default(),
            liquidity: Liquidity::default(),
            venue_time: None,
            recv_time: NanoTime::default(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Any `Copy + Default` identity will do; nothing here reads it.
    type Instrument = u32;
    type Order = super::Order<Instrument>;
    type Fill = super::Fill<Instrument>;

    /// The layout: the epoch above the sequence, both read back,
    /// and the widest id fits under 2⁵¹ — exact as a JSON double.
    #[test]
    fn an_id_carries_its_epoch_above_its_sequence() {
        let id = ClientOrderId::new(Epoch::new(3).unwrap(), 42);
        assert_eq!(id.0, (3 << 32) | 42);
        assert_eq!(id.epoch(), Epoch::new(3).unwrap());
        assert_eq!(id.sequence(), 42);
        assert_eq!(ClientOrderId::new(Epoch::ZERO, 9), ClientOrderId(9));
        assert!(ClientOrderId::new(Epoch::MAX, u32::MAX).0 < 1 << 51);
        assert_eq!(Epoch::new(Epoch::MAX.get() + 1), None);
    }

    /// An epoch steps, wraps past the top to one and never to zero, and one
    /// out of the clock is never zero either.
    #[test]
    fn an_epoch_steps_and_never_lands_on_zero() {
        assert_eq!(Epoch::ZERO.next().get(), 1);
        assert_eq!(Epoch::new(5).unwrap().next().get(), 6);
        assert_eq!(Epoch::MAX.next().get(), 1);
        for secs in [
            0,
            1,
            u64::from(Epoch::MAX.get()) - 1,
            u64::from(Epoch::MAX.get()),
            1_790_000_000,
        ] {
            let epoch = Epoch::from_clock(secs);
            assert_ne!(epoch, Epoch::ZERO);
            assert!(epoch <= Epoch::MAX);
        }
    }
    use crate::Burst;

    /// The reason the instrument is a `Copy` id and not a symbol. An order that
    /// allocates is an order that allocates on every hop of the feedback edge
    /// it rides, and the edge is the hot path the whole quoting loop sits on.
    ///
    /// A fill is the same value shape for a different reason: it is what the
    /// position fold holds, and one allocating field would be enough to make
    /// that fold allocate per execution.
    #[test]
    fn orders_and_fills_are_plain_values() {
        fn assert_copy<T: Copy>() {}
        assert_copy::<Order>();
        assert_copy::<Fill>();
    }

    /// Both need a `Default` to ride a `Burst<T>` — a `TinyVec<[T; 1]>`
    /// requires one — which is the only reason they have one.
    #[test]
    fn they_can_ride_a_burst() {
        let mut orders: Burst<Order> = Burst::new();
        orders.push(Order::default());
        assert_eq!(orders.len(), 1);
        let fills: Burst<Fill> = Burst::new();
        assert!(fills.is_empty());
    }

    /// The property that makes a `Default` safe to have at all. A defaulted
    /// order that leaked into the OMS is refused by any venue on the zero
    /// quantity, and cannot cross even if one were not: it never reads as a
    /// live buy of something.
    #[test]
    fn the_default_order_is_inert() {
        let order = Order::default();
        assert_eq!(
            order.qty,
            Qty::default(),
            "a zero-quantity order is refused"
        );
        assert_eq!(order.price, None, "no price to cross at");
        assert_eq!(
            order.kind,
            OrderKind::PostOnly,
            "the default kind must be the one that cannot cross"
        );
        assert!(!order.reduce_only);
    }

    /// The same for a fill: nothing about it reads as a real execution, and
    /// the id it would reconcile on is empty rather than plausible.
    #[test]
    fn the_default_fill_is_not_an_execution() {
        let fill = Fill::default();
        assert!(fill.exec_id.is_empty());
        assert_eq!(fill.qty, Qty::default());
        assert_eq!(fill.filled, Qty::ZERO, "nothing has filled on no order");
        assert_eq!(fill.price, Px::default());
        assert_eq!(fill.fee, Qty::ZERO, "a placeholder was charged nothing");
        assert_eq!(
            fill.liquidity,
            Liquidity::Maker,
            "a defaulted order is post-only, so maker is the inert side"
        );
    }

    /// A fee is an amount of the instrument's settlement currency, so it is a
    /// `Qty`. The type is the documentation: this compiles, and a fee that had
    /// stayed a `Px` would invite being compared with `price` — which is a
    /// price of the contract, in an entirely different unit.
    #[test]
    fn a_fee_is_an_amount_not_a_price() {
        let fill = Fill {
            fee: Qty::parse("-0.0000125").unwrap(),
            liquidity: Liquidity::Maker,
            ..Default::default()
        };
        assert!(fill.fee < Qty::ZERO, "a maker rebate is a negative fee");
        let taker = Fill {
            fee: Qty::parse("0.0003").unwrap(),
            liquidity: Liquidity::Taker,
            ..Default::default()
        };
        assert_eq!(taker.liquidity, Liquidity::Taker);
    }

    fn spec() -> Instrument {
        7
    }

    fn sendable() -> Order {
        Order::post_only(
            ClientOrderId(7),
            spec(),
            Side::Bid,
            Qty::parse("10").unwrap(),
            Px::parse("0.05").unwrap(),
        )
    }

    /// The constructors are what make `validate` a backstop rather than the
    /// only thing standing between the quoter and a venue rejection: each one
    /// can only produce an agreeing set of `kind`, `price` and `tif`.
    #[test]
    fn every_constructor_builds_a_sendable_order() {
        let qty = Qty::parse("10").unwrap();
        let px = Px::parse("0.05").unwrap();

        let limit = Order::limit(ClientOrderId(1), spec(), Side::Bid, qty, px);
        limit.validate().unwrap();
        assert_eq!((limit.kind, limit.price), (OrderKind::Limit, Some(px)));
        assert_eq!(limit.tif, TimeInForce::GoodTillCancel);

        let post_only = Order::post_only(ClientOrderId(2), spec(), Side::Ask, qty, px);
        post_only.validate().unwrap();
        assert_eq!(post_only.kind, OrderKind::PostOnly);
        assert_eq!(
            post_only.tif,
            TimeInForce::GoodTillCancel,
            "post-only with an immediate lifetime can only ever cancel"
        );

        // No price to be ignored, and nothing left to rest as if it does not
        // cross now.
        let market = Order::market(ClientOrderId(3), spec(), Side::Bid, qty);
        market.validate().unwrap();
        assert_eq!((market.kind, market.price), (OrderKind::Market, None));
        assert_eq!(market.tif, TimeInForce::ImmediateOrCancel);

        for order in [limit, post_only, market] {
            assert_eq!(order.qty, qty);
            assert!(!order.reduce_only, "the caller asks for that deliberately");
        }
        // Ids stay distinct values, which is what defuses `TimeQueue` dedup.
        assert_ne!(limit.id, post_only.id);
    }

    #[test]
    fn a_well_formed_order_validates() {
        sendable().validate().unwrap();
        assert!(sendable().is_well_formed());
    }

    /// The default exists for `Burst`, not to be sent. Validation is the other
    /// half of "inert": the zero quantity is caught here rather than at a venue.
    #[test]
    fn the_default_order_does_not_validate() {
        assert_eq!(
            Order::default().validate(),
            Err(OrderError::NonPositiveQuantity(Qty::ZERO))
        );
        assert!(!Order::default().is_well_formed());
    }

    /// The three ways `kind`, `price` and `tif` can contradict each other —
    /// reachable only by editing an order after a constructor built it.
    #[test]
    fn price_and_kind_and_tif_have_to_agree() {
        let price = Px::parse("0.05").unwrap();
        let market_with_price = Order {
            price: Some(price),
            ..Order::market(ClientOrderId(1), spec(), Side::Bid, sendable().qty)
        };
        assert_eq!(
            market_with_price.validate(),
            Err(OrderError::MarketWithPrice(price))
        );

        for kind in [OrderKind::Limit, OrderKind::PostOnly] {
            let resting_without_price = Order {
                kind,
                price: None,
                ..sendable()
            };
            assert_eq!(
                resting_without_price.validate(),
                Err(OrderError::RestingWithoutPrice(kind)),
                "{kind:?} with no price has no level to rest at"
            );
        }

        for tif in [TimeInForce::ImmediateOrCancel, TimeInForce::FillOrKill] {
            let post_only_crossing = Order { tif, ..sendable() };
            assert_eq!(
                post_only_crossing.validate(),
                Err(OrderError::PostOnlyCannotCross(tif))
            );
        }

        // A limit order may be IOC — it crosses, it just names a bound.
        Order {
            tif: TimeInForce::ImmediateOrCancel,
            ..Order::limit(ClientOrderId(1), spec(), Side::Bid, sendable().qty, price)
        }
        .validate()
        .unwrap();
    }

    /// Negative quantity is not a short — side says that — so it is refused
    /// alongside zero.
    #[test]
    fn a_negative_quantity_is_refused_like_a_zero_one() {
        let negative = Qty::parse("-1").unwrap();
        assert_eq!(
            Order {
                qty: negative,
                ..sendable()
            }
            .validate(),
            Err(OrderError::NonPositiveQuantity(negative))
        );
    }

    /// The one statement of which way each trigger fires: a buy stop at or
    /// above its price, a sell stop at or below, a take the other way.
    #[test]
    fn a_trigger_fires_the_way_its_kind_says() {
        let price = Px::parse("100").unwrap();
        let (above, at, below) = (Px::parse("101").unwrap(), price, Px::parse("99").unwrap());
        let stop = Trigger::stop(Reference::Mark, price);
        let take = Trigger::take(Reference::Mark, price);
        for (trigger, side, fires_at) in [
            (stop, Side::Bid, [true, true, false]),
            (stop, Side::Ask, [false, true, true]),
            (take, Side::Bid, [false, true, true]),
            (take, Side::Ask, [true, true, false]),
        ] {
            let fired = [above, at, below].map(|reference| trigger.fires(side, reference));
            assert_eq!(fired, fires_at, "{:?} {side:?}", trigger.kind);
        }
    }

    /// A triggered order rests until it fires, so it is neither post-only
    /// nor an immediate lifetime; a triggered limit or market order is
    /// sendable.
    #[test]
    fn a_triggered_order_rests_and_may_cross() {
        let trigger = Some(Trigger::stop(Reference::Mark, Px::parse("0.04").unwrap()));
        let limit = Order {
            trigger,
            ..Order::limit(
                ClientOrderId(1),
                spec(),
                Side::Ask,
                sendable().qty,
                Px::parse("0.039").unwrap(),
            )
        };
        limit.validate().unwrap();
        assert_eq!(
            Order {
                trigger,
                ..sendable()
            }
            .validate(),
            Err(OrderError::TriggeredPostOnly)
        );
        assert_eq!(
            Order {
                tif: TimeInForce::ImmediateOrCancel,
                ..limit
            }
            .validate(),
            Err(OrderError::TriggeredImmediate(
                TimeInForce::ImmediateOrCancel
            ))
        );
        // A stop that becomes a market order once it fires: an IOC of its
        // own making is the market constructor's, refused under a trigger.
        let market = Order {
            trigger,
            tif: TimeInForce::GoodTillCancel,
            ..Order::market(ClientOrderId(2), spec(), Side::Ask, sendable().qty)
        };
        market.validate().unwrap();
        assert_eq!(
            Order::default().trigger,
            None,
            "a defaulted order is live, not armed"
        );
    }
}
