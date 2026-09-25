//! A simulated **exchange**: a central limit order book where every order
//! comes from a participant and every fill is against another participant.
//!
//! [`ExchangeOps::exchange`] takes one stream of participant [`Request`]s —
//! new orders, cancels and amends, each tagged with an [`AccountId`] — and
//! emits an [`ExchangeOutput`] per cycle: the private [`Report`]s for every
//! account it touched, and the public [`MarketEvent`]s (trade prints and book
//! snapshots) that any participant may see. [`LedgerOps::ledger`] folds the
//! reports into per-account [`Position`]s.
//!
//! # How this differs from [`sim`](super::sim)
//!
//! `sim` matches **one** strategy against a **replayed** book: the liquidity
//! is history, it is never consumed, and nobody is on the other side of a
//! fill. That is the right model for a backtest against recorded data.
//!
//! This module has no book to replay. **Liquidity only comes from
//! participants.** An order rests until another participant's order crosses
//! it; a fill takes the resting quantity away; queue position is real, not
//! modelled. That is the right model for anything where the participants *are*
//! the market — an arena, a market-making simulation, an agent-based study.
//! Price discovery is whatever the participants do; anchoring it to an
//! external index is the job of the participants (or of funding and margin,
//! which live above this module).
//!
//! # Scope, stated so it can be held
//!
//! `docs/planning/proposals/trading-stack.md` §13 is the scope ruling for this
//! module. In: continuous price-time matching, limit and market orders, the
//! four [`TimeInForce`]s, cancel, amend, maker/taker fees, self-trade
//! prevention, a public trade and book feed. Out: auctions, iceberg / hidden /
//! pegged / stop orders, sessions (`Day` works as `GoodTillCancel`), pro-rata
//! allocation. Margin, funding, liquidation and latency are separate layers.
//!
//! # The decisions this exchange makes explicitly
//!
//! **1. Requests are processed in arrival order, one at a time.** Within a
//! [`Burst`] the order is the burst's order; nothing is batched, netted or
//! re-sequenced. Two requests that land in the same instant are still
//! sequenced — the burst *is* the sequence.
//!
//! **2. Price-time priority.** Better price first; at a price, first to arrive
//! first. An amend that raises quantity or moves the price loses its place and
//! re-enters at the back (and may trade on the way in); an amend that only
//! lowers quantity keeps it.
//!
//! **3. Trades print at the resting order's price.** The aggressor gets any
//! price improvement.
//!
//! **4. Self-trade prevention cancels the resting order.** An aggressor that
//! would cross its own account's resting order cancels that order
//! ([`CancelReason::SelfTrade`]) and carries on down the book. Fill-or-kill
//! counts only liquidity it could actually trade with.
//!
//! **5. Participants get rejected, not the run.** A malformed order from a
//! participant is a [`Report::Rejected`], never an `Err` — one bad request
//! must not stop a venue that other accounts are trading on. An `Err` from
//! this op means the exchange itself is broken (fixed-point overflow).
//!
//! **6. Fees round against the participant.** A cost is rounded up to the
//! next representable unit, a rebate is rounded down toward zero.

use std::collections::{BTreeMap, HashMap, VecDeque};
use std::sync::Arc;

use anyhow::Result;

use super::{
    AccountId, ClOrdId, ExecId, Fill, Notional, Order, OrderType, Position, TimeInForce,
    notional_of,
};
use crate::adapters::market::{
    BookSnapshot, BookUpdate, InstrumentId, Level, MarketEvent, Px, Qty, Sequencing, Side, Trade,
};
use crate::fluent::{Stream, StreamOps};
use crate::op::{Activation, Ctx, Op, Tick};
use crate::{Burst, NanoTime, op};

// -------------------------------------------------------------------------
// Configuration.
// -------------------------------------------------------------------------

/// A fee rate in parts per million of notional. Positive is a cost, negative a
/// rebate. One basis point is 100 ppm.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct FeeRate(i64);

impl FeeRate {
    /// No fee.
    pub const ZERO: Self = Self(0);

    /// A rate in parts per million of notional.
    pub const fn ppm(ppm: i64) -> Self {
        Self(ppm)
    }

    /// A rate in basis points of notional.
    pub const fn bps(bps: i64) -> Self {
        Self(bps * 100)
    }

    /// The rate in parts per million.
    pub const fn as_ppm(self) -> i64 {
        self.0
    }

    /// The fee on `notional`, rounded against the participant: a cost rounds
    /// up, a rebate rounds toward zero.
    ///
    /// # Errors
    ///
    /// If the product overflows the fixed-point range.
    pub fn charge(self, notional: Notional) -> Result<Notional> {
        let product = notional
            .raw()
            .checked_mul(i128::from(self.0))
            .ok_or_else(|| anyhow::anyhow!("fee on {notional} at {} ppm overflows", self.0))?;
        let fee = if product > 0 {
            (product + 999_999) / 1_000_000
        } else {
            product / 1_000_000
        };
        Ok(Notional::from_raw(fee))
    }
}

/// Maker and taker fee rates.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct FeeSchedule {
    /// Charged to the resting side of a fill.
    pub maker: FeeRate,
    /// Charged to the aggressing side of a fill.
    pub taker: FeeRate,
}

/// What the exchange lists and on what terms.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct InstrumentSpec {
    /// The instrument, as it appears on every order, fill and print.
    pub id: InstrumentId,
    /// Minimum price increment. Limit prices must be a whole multiple.
    pub tick: Px,
    /// Minimum quantity increment. Quantities must be a whole multiple.
    pub lot: Qty,
    /// Fees charged on this instrument's fills.
    pub fees: FeeSchedule,
}

impl InstrumentSpec {
    /// An instrument with the given tick and lot and no fees.
    pub fn new(id: InstrumentId, tick: Px, lot: Qty) -> Self {
        Self {
            id,
            tick,
            lot,
            fees: FeeSchedule::default(),
        }
    }

    /// Set the fee schedule.
    pub fn with_fees(mut self, fees: FeeSchedule) -> Self {
        self.fees = fees;
        self
    }
}

/// The exchange's construction-time configuration.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ExchangeConfig {
    /// Prefix for execution and trade ids.
    pub venue: String,
    /// Listed instruments. An order for anything else is rejected.
    pub instruments: Vec<InstrumentSpec>,
    /// Levels per side in the public book snapshots. `0` publishes no book.
    pub book_depth: usize,
}

impl ExchangeConfig {
    /// A venue listing `instruments`, publishing 10 levels a side.
    pub fn new(venue: impl Into<String>, instruments: Vec<InstrumentSpec>) -> Self {
        Self {
            venue: venue.into(),
            instruments,
            book_depth: 10,
        }
    }

    /// Set the public book depth.
    pub fn with_book_depth(mut self, depth: usize) -> Self {
        self.book_depth = depth;
        self
    }
}

// -------------------------------------------------------------------------
// Requests and reports.
// -------------------------------------------------------------------------

/// A participant's instruction to the exchange.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Request {
    /// Submit a new order. Its [`ClOrdId`] must not collide with one of the
    /// same account's live orders.
    New {
        /// The account it concerns.
        account: AccountId,
        /// The order.
        order: Order,
    },
    /// Cancel a live order.
    Cancel {
        /// The account it concerns.
        account: AccountId,
        /// The order it concerns.
        cl_ord_id: ClOrdId,
    },
    /// Change a live limit order's total quantity and/or price. `None` leaves
    /// that field unchanged. The new quantity is the order's *total*, filled
    /// part included, and must exceed what has already filled.
    Amend {
        /// The account it concerns.
        account: AccountId,
        /// The order it concerns.
        cl_ord_id: ClOrdId,
        /// New total quantity.
        qty: Option<Qty>,
        /// New limit price.
        price: Option<Px>,
    },
}

/// The engine's value-slot requirement, not a meaningful request.
impl Default for Request {
    fn default() -> Self {
        Request::Cancel {
            account: AccountId::default(),
            cl_ord_id: ClOrdId::default(),
        }
    }
}

impl Request {
    /// The account the request is from.
    pub fn account(&self) -> &AccountId {
        match self {
            Request::New { account, .. }
            | Request::Cancel { account, .. }
            | Request::Amend { account, .. } => account,
        }
    }
}

/// Which side of a fill an account was on.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub enum Liquidity {
    /// The order was resting.
    #[default]
    Maker,
    /// The order crossed the book.
    Taker,
}

/// Why a request was refused.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub enum RejectReason {
    /// The order failed [`Order::validate`]; the text says why.
    #[default]
    Invalid,
    /// The instrument is not listed.
    UnknownInstrument,
    /// The price is not a multiple of the instrument's tick.
    OffTick,
    /// The quantity is not a multiple of the instrument's lot.
    OffLot,
    /// The account already has a live order with this id.
    DuplicateClOrdId,
    /// No live order with this id for this account.
    UnknownOrder,
    /// An amend that is not allowed: a market order, or a total quantity at
    /// or below what has already filled.
    BadAmend,
}

/// Why a live order stopped working without filling in full.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub enum CancelReason {
    /// The account asked.
    #[default]
    Requested,
    /// A market or immediate-or-cancel order's unfilled remainder.
    Unfilled,
    /// A fill-or-kill order that could not fill in full.
    FillOrKill,
    /// Cancelled by self-trade prevention (decision 4).
    SelfTrade,
}

/// What the exchange tells one account about its own orders.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Report {
    /// A new order passed validation and is live (it may fill at once).
    Accepted {
        /// The account it concerns.
        account: AccountId,
        /// The order it concerns.
        cl_ord_id: ClOrdId,
        /// Engine time the exchange processed it.
        time: NanoTime,
    },
    /// A request was refused; nothing changed.
    Rejected {
        /// The account it concerns.
        account: AccountId,
        /// The order it concerns.
        cl_ord_id: ClOrdId,
        /// Why.
        reason: RejectReason,
        /// Human-readable detail.
        text: Arc<str>,
        /// Engine time the exchange processed it.
        time: NanoTime,
    },
    /// Part or all of an order traded.
    Filled {
        /// The account it concerns.
        account: AccountId,
        /// The execution.
        fill: Fill,
        /// Maker or taker.
        liquidity: Liquidity,
    },
    /// A live order stopped working with `leaves_qty` unfilled.
    Cancelled {
        /// The account it concerns.
        account: AccountId,
        /// The order it concerns.
        cl_ord_id: ClOrdId,
        /// Quantity still unfilled.
        leaves_qty: Qty,
        /// Why.
        reason: CancelReason,
        /// Engine time the exchange processed it.
        time: NanoTime,
    },
    /// A live order's quantity and/or price changed.
    Amended {
        /// The account it concerns.
        account: AccountId,
        /// The order it concerns.
        cl_ord_id: ClOrdId,
        /// The order's total quantity now.
        qty: Qty,
        /// The order's limit price now.
        price: Px,
        /// Quantity still unfilled.
        leaves_qty: Qty,
        /// Engine time the exchange processed it.
        time: NanoTime,
    },
}

/// The engine's value-slot requirement, not a meaningful report.
impl Default for Report {
    fn default() -> Self {
        Report::Rejected {
            account: AccountId::default(),
            cl_ord_id: ClOrdId::default(),
            reason: RejectReason::default(),
            text: Arc::from(""),
            time: NanoTime::ZERO,
        }
    }
}

impl Report {
    /// The account the report is for.
    pub fn account(&self) -> &AccountId {
        match self {
            Report::Accepted { account, .. }
            | Report::Rejected { account, .. }
            | Report::Filled { account, .. }
            | Report::Cancelled { account, .. }
            | Report::Amended { account, .. } => account,
        }
    }
}

/// One cycle's output: private reports and public market data.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ExchangeOutput {
    /// Reports for every account this cycle touched, in the order they
    /// happened. Route by [`Report::account`].
    pub reports: Burst<Report>,
    /// Trade prints, then one book snapshot per instrument whose book changed.
    pub market: Burst<MarketEvent>,
}

// -------------------------------------------------------------------------
// The book.
// -------------------------------------------------------------------------

#[derive(Clone, Debug)]
struct Resting {
    account: AccountId,
    order: Order,
    cum_qty: Qty,
    leaves_qty: Qty,
}

#[derive(Debug)]
struct Book {
    spec: InstrumentSpec,
    /// Keyed by price; the best bid is the *last* key, the best ask the first.
    bids: BTreeMap<Px, VecDeque<Resting>>,
    asks: BTreeMap<Px, VecDeque<Resting>>,
    seq: u64,
    changed: bool,
}

impl Book {
    fn new(spec: InstrumentSpec) -> Self {
        Self {
            spec,
            bids: BTreeMap::new(),
            asks: BTreeMap::new(),
            seq: 0,
            changed: false,
        }
    }

    fn side_mut(&mut self, side: Side) -> &mut BTreeMap<Px, VecDeque<Resting>> {
        match side {
            Side::Bid => &mut self.bids,
            Side::Ask => &mut self.asks,
        }
    }

    /// The best price on `side`.
    #[cfg(test)]
    fn best(&self, side: Side) -> Option<Px> {
        match side {
            Side::Bid => self.bids.keys().next_back().copied(),
            Side::Ask => self.asks.keys().next().copied(),
        }
    }

    /// Prices on `side` from the touch outward.
    fn prices(&self, side: Side) -> Vec<Px> {
        match side {
            Side::Bid => self.bids.keys().rev().copied().collect(),
            Side::Ask => self.asks.keys().copied().collect(),
        }
    }

    fn rest(&mut self, resting: Resting) {
        let price = resting
            .order
            .price
            .expect("invariant: only limit orders rest");
        self.side_mut(resting.order.side)
            .entry(price)
            .or_default()
            .push_back(resting);
        self.changed = true;
    }

    /// Remove a resting order, returning it.
    fn remove(
        &mut self,
        side: Side,
        price: Px,
        account: &AccountId,
        id: &ClOrdId,
    ) -> Option<Resting> {
        let levels = self.side_mut(side);
        let queue = levels.get_mut(&price)?;
        let at = queue
            .iter()
            .position(|r| &r.account == account && &r.order.cl_ord_id == id)?;
        let resting = queue.remove(at);
        if queue.is_empty() {
            levels.remove(&price);
        }
        self.changed = true;
        resting
    }

    fn snapshot(&mut self, depth: usize, now: NanoTime) -> BookSnapshot {
        let aggregate = |queue: &VecDeque<Resting>| {
            Qty::from_raw(queue.iter().map(|r| r.leaves_qty.raw()).sum())
        };
        self.seq += 1;
        BookSnapshot {
            instrument: self.spec.id.clone(),
            bids: self
                .bids
                .iter()
                .rev()
                .take(depth)
                .map(|(p, q)| Level::new(*p, aggregate(q)))
                .collect(),
            asks: self
                .asks
                .iter()
                .take(depth)
                .map(|(p, q)| Level::new(*p, aggregate(q)))
                .collect(),
            sequencing: Sequencing::Single(self.seq),
            venue_time: Some(now),
            recv_time: now,
        }
    }
}

/// Where a live order sits, so a cancel or amend finds it without a scan.
#[derive(Clone, Debug)]
struct Locator {
    instrument: InstrumentId,
    side: Side,
    price: Px,
}

/// The exchange's engine-owned state.
#[derive(Debug, Default)]
pub struct ExchangeState {
    books: HashMap<InstrumentId, Book>,
    live: HashMap<(AccountId, ClOrdId), Locator>,
    next_exec_id: u64,
    next_trade_id: u64,
}

impl ExchangeState {
    /// Live (resting) orders across all instruments. For tests and diagnostics.
    pub fn live_orders(&self) -> usize {
        self.live.len()
    }
}

// -------------------------------------------------------------------------
// The op.
// -------------------------------------------------------------------------

/// Matches participant requests against each other — see the module docs.
pub struct ExchangeOp;

#[op(build = exchange)]
impl Op for ExchangeOp {
    type Cfg = ExchangeConfig;
    type State = ExchangeState;
    type In<'a> = (&'a Burst<Request>,);
    type Out = ExchangeOutput;
    const ACTIVATION: Activation = Activation::NONE;

    fn cycle(
        cfg: &mut ExchangeConfig,
        state: &mut ExchangeState,
        input: (&Burst<Request>,),
        ctx: &mut Ctx<'_>,
    ) -> Result<Tick<ExchangeOutput>> {
        let now = ctx.time();
        let mut out = ExchangeOutput::default();
        for request in input.0.iter() {
            process(cfg, state, request, now, &mut out)?;
        }
        if cfg.book_depth > 0 {
            for book in state.books.values_mut().filter(|b| b.changed) {
                book.changed = false;
                out.market.push(MarketEvent::Book(BookUpdate::Snapshot(
                    book.snapshot(cfg.book_depth, now),
                )));
            }
        }
        Ok(if out.reports.is_empty() && out.market.is_empty() {
            Tick::Quiet
        } else {
            Tick::Value(out)
        })
    }
}

/// Everything one cycle needs to emit reports and prints.
struct Emit<'a> {
    venue: &'a str,
    now: NanoTime,
    out: &'a mut ExchangeOutput,
}

impl Emit<'_> {
    fn reject(
        &mut self,
        account: &AccountId,
        id: &ClOrdId,
        reason: RejectReason,
        text: impl Into<Arc<str>>,
    ) {
        self.out.reports.push(Report::Rejected {
            account: account.clone(),
            cl_ord_id: id.clone(),
            reason,
            text: text.into(),
            time: self.now,
        });
    }

    fn cancelled(
        &mut self,
        account: &AccountId,
        id: &ClOrdId,
        leaves_qty: Qty,
        reason: CancelReason,
    ) {
        self.out.reports.push(Report::Cancelled {
            account: account.clone(),
            cl_ord_id: id.clone(),
            leaves_qty,
            reason,
            time: self.now,
        });
    }
}

fn process(
    cfg: &ExchangeConfig,
    state: &mut ExchangeState,
    request: &Request,
    now: NanoTime,
    out: &mut ExchangeOutput,
) -> Result<()> {
    let mut emit = Emit {
        venue: &cfg.venue,
        now,
        out,
    };
    match request {
        Request::New { account, order } => new_order(cfg, state, account, order.clone(), &mut emit),
        Request::Cancel { account, cl_ord_id } => {
            let key = (account.clone(), cl_ord_id.clone());
            let Some(loc) = state.live.remove(&key) else {
                emit.reject(
                    account,
                    cl_ord_id,
                    RejectReason::UnknownOrder,
                    "no live order with this id",
                );
                return Ok(());
            };
            let book = state
                .books
                .get_mut(&loc.instrument)
                .expect("invariant: live order has a book");
            let resting = book
                .remove(loc.side, loc.price, account, cl_ord_id)
                .expect("invariant: live order is on its book");
            emit.cancelled(
                account,
                cl_ord_id,
                resting.leaves_qty,
                CancelReason::Requested,
            );
            Ok(())
        }
        Request::Amend {
            account,
            cl_ord_id,
            qty,
            price,
        } => amend(state, account, cl_ord_id, *qty, *price, &mut emit),
    }
}

/// Check an order against its instrument's terms.
fn check(spec: &InstrumentSpec, order: &Order) -> Result<(), (RejectReason, String)> {
    if let Err(e) = order.validate() {
        return Err((RejectReason::Invalid, e.to_string()));
    }
    if !multiple_of(order.qty.raw(), spec.lot.raw()) {
        return Err((
            RejectReason::OffLot,
            format!("qty {} is not a multiple of lot {}", order.qty, spec.lot),
        ));
    }
    if let Some(price) = order.price
        && !multiple_of(price.raw(), spec.tick.raw())
    {
        return Err((
            RejectReason::OffTick,
            format!("price {price} is not a multiple of tick {}", spec.tick),
        ));
    }
    Ok(())
}

fn multiple_of(value: i128, step: i128) -> bool {
    step <= 0 || value % step == 0
}

fn new_order(
    cfg: &ExchangeConfig,
    state: &mut ExchangeState,
    account: &AccountId,
    order: Order,
    emit: &mut Emit<'_>,
) -> Result<()> {
    let id = &order.cl_ord_id;
    let Some(spec) = cfg
        .instruments
        .iter()
        .find(|s| s.id.same_as(&order.instrument))
    else {
        emit.reject(
            account,
            id,
            RejectReason::UnknownInstrument,
            format!("{} is not listed", order.instrument),
        );
        return Ok(());
    };
    if let Err((reason, text)) = check(spec, &order) {
        emit.reject(account, id, reason, text);
        return Ok(());
    }
    if state.live.contains_key(&(account.clone(), id.clone())) {
        emit.reject(
            account,
            id,
            RejectReason::DuplicateClOrdId,
            "a live order already has this id",
        );
        return Ok(());
    }
    emit.out.reports.push(Report::Accepted {
        account: account.clone(),
        cl_ord_id: id.clone(),
        time: emit.now,
    });
    let book = state
        .books
        .entry(spec.id.clone())
        .or_insert_with(|| Book::new(spec.clone()));
    let incoming = Resting {
        account: account.clone(),
        leaves_qty: order.qty,
        cum_qty: Qty::ZERO,
        order,
    };
    let (ids, live) = (&mut state.next_exec_id, &mut state.live);
    enter(book, incoming, live, &mut state.next_trade_id, ids, emit)
}

/// Match an incoming order and rest (or cancel) whatever is left.
fn enter(
    book: &mut Book,
    mut incoming: Resting,
    live: &mut HashMap<(AccountId, ClOrdId), Locator>,
    next_trade_id: &mut u64,
    next_exec_id: &mut u64,
    emit: &mut Emit<'_>,
) -> Result<()> {
    let tif = incoming.order.time_in_force;
    if tif == TimeInForce::FillOrKill && tradeable(book, &incoming) < incoming.leaves_qty.raw() {
        let (account, id) = (incoming.account.clone(), incoming.order.cl_ord_id.clone());
        emit.cancelled(&account, &id, incoming.leaves_qty, CancelReason::FillOrKill);
        return Ok(());
    }

    let taking = incoming.order.side.opposite();
    for price in book.prices(taking) {
        if incoming.leaves_qty.is_zero() || !crosses(&incoming.order, price) {
            break;
        }
        take_level(
            book,
            taking,
            price,
            &mut incoming,
            live,
            next_trade_id,
            next_exec_id,
            emit,
        )?;
    }

    let (account, id) = (incoming.account.clone(), incoming.order.cl_ord_id.clone());
    if incoming.leaves_qty.is_zero() {
        return Ok(());
    }
    if rests(&incoming.order) {
        let price = incoming
            .order
            .price
            .expect("invariant: a resting order is a limit order");
        live.insert(
            (account, id),
            Locator {
                instrument: book.spec.id.clone(),
                side: incoming.order.side,
                price,
            },
        );
        book.rest(incoming);
    } else {
        emit.cancelled(&account, &id, incoming.leaves_qty, CancelReason::Unfilled);
    }
    Ok(())
}

/// How much of the book `incoming` could actually trade with — self-trade
/// prevention would cancel its own account's orders rather than fill them.
fn tradeable(book: &Book, incoming: &Resting) -> i128 {
    let taking = incoming.order.side.opposite();
    let levels = match taking {
        Side::Bid => &book.bids,
        Side::Ask => &book.asks,
    };
    let walk: Box<dyn Iterator<Item = (&Px, &VecDeque<Resting>)>> = match taking {
        Side::Bid => Box::new(levels.iter().rev()),
        Side::Ask => Box::new(levels.iter()),
    };
    walk.take_while(|(p, _)| crosses(&incoming.order, **p))
        .flat_map(|(_, q)| q.iter())
        .filter(|r| r.account != incoming.account)
        .map(|r| r.leaves_qty.raw())
        .sum()
}

#[allow(clippy::too_many_arguments)]
fn take_level(
    book: &mut Book,
    taking: Side,
    price: Px,
    incoming: &mut Resting,
    live: &mut HashMap<(AccountId, ClOrdId), Locator>,
    next_trade_id: &mut u64,
    next_exec_id: &mut u64,
    emit: &mut Emit<'_>,
) -> Result<()> {
    let fees = book.spec.fees;
    let instrument = book.spec.id.clone();
    let queue = book
        .side_mut(taking)
        .get_mut(&price)
        .expect("invariant: price came from this side");
    while let Some(front) = queue.front_mut() {
        if incoming.leaves_qty.is_zero() {
            break;
        }
        if front.account == incoming.account {
            // Decision 4: cancel the resting order, keep going.
            let resting = queue.pop_front().expect("invariant: front exists");
            live.remove(&(resting.account.clone(), resting.order.cl_ord_id.clone()));
            emit.cancelled(
                &resting.account,
                &resting.order.cl_ord_id,
                resting.leaves_qty,
                CancelReason::SelfTrade,
            );
            continue;
        }
        let qty = Qty::from_raw(incoming.leaves_qty.raw().min(front.leaves_qty.raw()));
        let notional = notional_of(price, qty)?;

        *next_trade_id += 1;
        emit.out.market.push(MarketEvent::Trade(Trade {
            instrument: instrument.clone(),
            price,
            qty,
            aggressor: Some(incoming.order.side),
            trade_id: Some(Arc::from(format!("{}-t{}", emit.venue, next_trade_id))),
            venue_time: Some(emit.now),
            recv_time: emit.now,
        }));
        let maker_fill = fill(
            front,
            qty,
            price,
            fees.maker.charge(notional)?,
            next_exec_id,
            emit,
        );
        emit.out.reports.push(Report::Filled {
            account: front.account.clone(),
            fill: maker_fill,
            liquidity: Liquidity::Maker,
        });
        let taker_fill = fill(
            incoming,
            qty,
            price,
            fees.taker.charge(notional)?,
            next_exec_id,
            emit,
        );
        emit.out.reports.push(Report::Filled {
            account: incoming.account.clone(),
            fill: taker_fill,
            liquidity: Liquidity::Taker,
        });

        if front.leaves_qty.is_zero() {
            let done = queue.pop_front().expect("invariant: front exists");
            live.remove(&(done.account, done.order.cl_ord_id));
        }
    }
    if queue.is_empty() {
        book.side_mut(taking).remove(&price);
    }
    book.changed = true;
    Ok(())
}

/// Apply `qty` to one side of a match and build its [`Fill`].
fn fill(
    order: &mut Resting,
    qty: Qty,
    price: Px,
    fee: Notional,
    next_exec_id: &mut u64,
    emit: &Emit<'_>,
) -> Fill {
    order.cum_qty = Qty::from_raw(order.cum_qty.raw() + qty.raw());
    order.leaves_qty = Qty::from_raw(order.leaves_qty.raw() - qty.raw());
    *next_exec_id += 1;
    Fill {
        exec_id: ExecId::new(format!("{}-e{}", emit.venue, next_exec_id)),
        cl_ord_id: order.order.cl_ord_id.clone(),
        instrument: order.order.instrument.clone(),
        side: order.order.side,
        qty,
        price,
        cum_qty: order.cum_qty,
        leaves_qty: order.leaves_qty,
        fee,
        venue_time: Some(emit.now),
        recv_time: emit.now,
    }
}

fn amend(
    state: &mut ExchangeState,
    account: &AccountId,
    id: &ClOrdId,
    qty: Option<Qty>,
    price: Option<Px>,
    emit: &mut Emit<'_>,
) -> Result<()> {
    let key = (account.clone(), id.clone());
    let Some(loc) = state.live.get(&key).cloned() else {
        emit.reject(
            account,
            id,
            RejectReason::UnknownOrder,
            "no live order with this id",
        );
        return Ok(());
    };
    let book = state
        .books
        .get_mut(&loc.instrument)
        .expect("invariant: live order has a book");
    let current = book
        .side_mut(loc.side)
        .get(&loc.price)
        .and_then(|q| {
            q.iter()
                .find(|r| &r.account == account && &r.order.cl_ord_id == id)
        })
        .expect("invariant: live order is on its book")
        .clone();

    let new_qty = qty.unwrap_or(current.order.qty);
    let new_price = price.unwrap_or(loc.price);
    if new_qty.raw() <= current.cum_qty.raw() {
        emit.reject(
            account,
            id,
            RejectReason::BadAmend,
            "new quantity must exceed the filled quantity",
        );
        return Ok(());
    }
    let mut amended = current.order.clone();
    amended.qty = new_qty;
    amended.price = Some(new_price);
    if let Err((reason, text)) = check(&book.spec, &amended) {
        emit.reject(account, id, reason, text);
        return Ok(());
    }
    let leaves = Qty::from_raw(new_qty.raw() - current.cum_qty.raw());
    emit.out.reports.push(Report::Amended {
        account: account.clone(),
        cl_ord_id: id.clone(),
        qty: new_qty,
        price: new_price,
        leaves_qty: leaves,
        time: emit.now,
    });

    let keeps_priority = new_price == loc.price && new_qty.raw() <= current.order.qty.raw();
    if keeps_priority {
        let queue = book
            .side_mut(loc.side)
            .get_mut(&loc.price)
            .expect("invariant: level exists");
        let r = queue
            .iter_mut()
            .find(|r| &r.account == account && &r.order.cl_ord_id == id)
            .expect("invariant: order exists");
        r.order.qty = new_qty;
        r.leaves_qty = leaves;
        book.changed = true;
        return Ok(());
    }

    // Decision 2: loses its place, re-enters at the back, may trade on the way.
    book.remove(loc.side, loc.price, account, id);
    state.live.remove(&key);
    let incoming = Resting {
        account: account.clone(),
        order: amended,
        cum_qty: current.cum_qty,
        leaves_qty: leaves,
    };
    let (ids, live) = (&mut state.next_exec_id, &mut state.live);
    enter(book, incoming, live, &mut state.next_trade_id, ids, emit)
}

/// Whether an order's remainder rests rather than being cancelled. `Day` has
/// no session to end, so it rests like `GoodTillCancel`.
fn rests(order: &Order) -> bool {
    order.order_type == OrderType::Limit && order.time_in_force.rests()
}

fn crosses(order: &Order, price: Px) -> bool {
    match order.price {
        None => true,
        Some(limit) => match order.side {
            Side::Bid => price <= limit,
            Side::Ask => price >= limit,
        },
    }
}

// -------------------------------------------------------------------------
// The ledger.
// -------------------------------------------------------------------------

/// Every account's [`Position`] in every instrument, folded from reports.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Ledger {
    positions: HashMap<AccountId, HashMap<InstrumentId, Position>>,
}

impl Ledger {
    /// Fold one report in. Only fills change positions.
    ///
    /// # Errors
    ///
    /// If the position arithmetic overflows.
    pub fn apply(&mut self, report: &Report) -> Result<()> {
        if let Report::Filled { account, fill, .. } = report {
            self.positions
                .entry(account.clone())
                .or_default()
                .entry(fill.instrument.clone())
                .or_insert_with(|| Position::new(fill.instrument.clone()))
                .apply(fill)?;
        }
        Ok(())
    }

    /// One account's position in one instrument, if it has ever traded it.
    pub fn position(&self, account: &AccountId, instrument: &InstrumentId) -> Option<&Position> {
        self.positions.get(account)?.get(instrument)
    }

    /// Every account that has traded.
    pub fn accounts(&self) -> impl Iterator<Item = &AccountId> {
        self.positions.keys()
    }

    /// One account's positions across instruments.
    pub fn positions(&self, account: &AccountId) -> impl Iterator<Item = &Position> {
        self.positions
            .get(account)
            .into_iter()
            .flat_map(|m| m.values())
    }
}

/// Folds a [`Ledger`] from report bursts.
pub struct LedgerOp;

#[op(build = exchange_ledger)]
impl Op for LedgerOp {
    type Cfg = ();
    type State = Arc<Ledger>;
    type In<'a> = (&'a Burst<Report>,);
    type Out = Arc<Ledger>;
    const ACTIVATION: Activation = Activation::NONE;

    fn cycle(
        _cfg: &mut (),
        state: &mut Arc<Ledger>,
        input: (&Burst<Report>,),
        _ctx: &mut Ctx<'_>,
    ) -> Result<Tick<Arc<Ledger>>> {
        let mut changed = false;
        for report in input
            .0
            .iter()
            .filter(|r| matches!(r, Report::Filled { .. }))
        {
            Arc::make_mut(state).apply(report)?;
            changed = true;
        }
        Ok(if changed {
            Tick::Value(Arc::clone(state))
        } else {
            Tick::Quiet
        })
    }
}

// -------------------------------------------------------------------------
// Wiring.
// -------------------------------------------------------------------------

/// The exchange, wired on a stream of participant requests.
///
/// Not in the [`prelude`](crate::prelude) — `use
/// wingfoil::adapters::execution::exchange::ExchangeOps;`.
pub trait ExchangeOps {
    /// Match these requests on an exchange configured by `config`.
    ///
    /// Ticks on any cycle that produced a report or a market event.
    fn exchange(&self, config: ExchangeConfig) -> Stream<ExchangeOutput>;
}

impl ExchangeOps for Stream<Burst<Request>> {
    fn exchange(&self, config: ExchangeConfig) -> Stream<ExchangeOutput> {
        self.wire(|b, h| b.exchange(h, config))
    }
}

/// Split an [`ExchangeOutput`] stream into its private and public halves.
pub trait ExchangeOutputOps {
    /// The reports, ticking only when there are some.
    fn reports(&self) -> Stream<Burst<Report>>;
    /// The public trade prints and book snapshots, ticking only when there
    /// are some.
    fn market_events(&self) -> Stream<Burst<MarketEvent>>;
}

impl ExchangeOutputOps for Stream<ExchangeOutput> {
    fn reports(&self) -> Stream<Burst<Report>> {
        self.filter_map(|o: &ExchangeOutput| (!o.reports.is_empty()).then(|| o.reports.clone()))
    }

    fn market_events(&self) -> Stream<Burst<MarketEvent>> {
        self.filter_map(|o: &ExchangeOutput| (!o.market.is_empty()).then(|| o.market.clone()))
    }
}

/// The ledger fold on a stream of reports.
pub trait LedgerOps {
    /// Fold reports into every account's positions. Ticks when a fill lands.
    fn ledger(&self) -> Stream<Arc<Ledger>>;
}

impl LedgerOps for Stream<Burst<Report>> {
    fn ledger(&self) -> Stream<Arc<Ledger>> {
        self.wire(|b, h| b.exchange_ledger(h))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn inst() -> InstrumentId {
        InstrumentId::new("arena", "BTC-PERP")
    }

    fn px(s: &str) -> Px {
        Px::parse(s).unwrap()
    }

    fn qty(s: &str) -> Qty {
        Qty::parse(s).unwrap()
    }

    fn cfg() -> ExchangeConfig {
        ExchangeConfig::new(
            "arena",
            vec![
                InstrumentSpec::new(inst(), px("0.5"), qty("0.001")).with_fees(FeeSchedule {
                    maker: FeeRate::bps(-1),
                    taker: FeeRate::bps(5),
                }),
            ],
        )
    }

    fn acct(s: &str) -> AccountId {
        AccountId::new(s)
    }

    fn limit(a: &str, id: &str, side: Side, q: &str, p: &str) -> Request {
        Request::New {
            account: acct(a),
            order: Order::limit(ClOrdId::new(id), inst(), side, qty(q), px(p)),
        }
    }

    fn market(a: &str, id: &str, side: Side, q: &str) -> Request {
        Request::New {
            account: acct(a),
            order: Order::market(ClOrdId::new(id), inst(), side, qty(q)),
        }
    }

    struct Venue {
        cfg: ExchangeConfig,
        state: ExchangeState,
    }

    impl Venue {
        fn new() -> Self {
            Self {
                cfg: cfg(),
                state: ExchangeState::default(),
            }
        }

        fn send(&mut self, requests: Vec<Request>) -> ExchangeOutput {
            let mut out = ExchangeOutput::default();
            for r in &requests {
                process(&self.cfg, &mut self.state, r, NanoTime::ZERO, &mut out).unwrap();
            }
            out
        }

        fn fills(out: &ExchangeOutput) -> Vec<(String, Liquidity, Fill)> {
            out.reports
                .iter()
                .filter_map(|r| match r {
                    Report::Filled {
                        account,
                        fill,
                        liquidity,
                    } => Some((account.as_str().to_string(), *liquidity, fill.clone())),
                    _ => None,
                })
                .collect()
        }

        fn book(&self) -> &Book {
            &self.state.books[&inst()]
        }
    }

    #[test]
    fn nothing_fills_without_a_counterparty() {
        let mut v = Venue::new();
        let out = v.send(vec![market("a", "1", Side::Bid, "1")]);
        assert!(Venue::fills(&out).is_empty());
        assert!(matches!(
            out.reports.last().unwrap(),
            Report::Cancelled {
                reason: CancelReason::Unfilled,
                ..
            }
        ));
    }

    #[test]
    fn a_crossing_order_trades_at_the_resting_price() {
        let mut v = Venue::new();
        v.send(vec![limit("maker", "m1", Side::Ask, "1", "100")]);
        let out = v.send(vec![limit("taker", "t1", Side::Bid, "0.4", "101")]);
        let fills = Venue::fills(&out);
        assert_eq!(fills.len(), 2);
        let (who, liq, f) = &fills[0];
        assert_eq!((who.as_str(), *liq), ("maker", Liquidity::Maker));
        assert_eq!(f.price, px("100"));
        assert_eq!(f.leaves_qty, qty("0.6"));
        let (who, liq, f) = &fills[1];
        assert_eq!((who.as_str(), *liq), ("taker", Liquidity::Taker));
        assert_eq!(f.price, px("100"));
        assert_eq!(f.side, Side::Bid);
        assert_eq!(f.leaves_qty, Qty::ZERO);
        // The rest of the maker's order is still there.
        assert_eq!(v.book().best(Side::Ask), Some(px("100")));
        assert_eq!(v.state.live_orders(), 1);
    }

    #[test]
    fn price_then_time_priority() {
        let mut v = Venue::new();
        v.send(vec![
            limit("a", "a1", Side::Ask, "1", "101"),
            limit("b", "b1", Side::Ask, "1", "100"),
            limit("c", "c1", Side::Ask, "1", "100"),
        ]);
        let out = v.send(vec![market("t", "t1", Side::Bid, "2.5")]);
        let makers: Vec<_> = Venue::fills(&out)
            .into_iter()
            .filter(|f| f.1 == Liquidity::Maker)
            .map(|f| (f.0, f.2.price, f.2.qty))
            .collect();
        assert_eq!(
            makers,
            vec![
                ("b".into(), px("100"), qty("1")),
                ("c".into(), px("100"), qty("1")),
                ("a".into(), px("101"), qty("0.5")),
            ]
        );
    }

    #[test]
    fn fees_are_maker_rebate_and_taker_cost_rounded_against_the_participant() {
        let mut v = Venue::new();
        v.send(vec![limit("m", "m1", Side::Bid, "0.003", "100.5")]);
        let out = v.send(vec![market("t", "t1", Side::Ask, "0.003")]);
        let fills = Venue::fills(&out);
        // Notional 0.3015; maker -1bp = -0.00003015, taker 5bp = 0.00015075.
        assert_eq!(fills[0].2.fee, Notional::parse("-0.00003015").unwrap());
        assert_eq!(fills[1].2.fee, Notional::parse("0.00015075").unwrap());
        // Sub-unit rounding: a cost rounds up, a rebate toward zero.
        let tiny = Notional::from_raw(1);
        assert_eq!(FeeRate::bps(5).charge(tiny).unwrap(), Notional::from_raw(1));
        assert_eq!(FeeRate::bps(-1).charge(tiny).unwrap(), Notional::ZERO);
    }

    #[test]
    fn self_trade_prevention_cancels_the_resting_order() {
        let mut v = Venue::new();
        v.send(vec![
            limit("a", "a1", Side::Ask, "1", "100"),
            limit("b", "b1", Side::Ask, "1", "100"),
        ]);
        let out = v.send(vec![limit("a", "a2", Side::Bid, "1", "100")]);
        assert!(out.reports.iter().any(|r| matches!(
            r,
            Report::Cancelled { cl_ord_id, reason: CancelReason::SelfTrade, .. } if cl_ord_id.as_str() == "a1"
        )));
        let fills = Venue::fills(&out);
        assert_eq!(fills[0].0, "b");
        assert_eq!(v.state.live_orders(), 0);
    }

    #[test]
    fn fill_or_kill_ignores_own_liquidity() {
        let mut v = Venue::new();
        v.send(vec![
            limit("a", "a1", Side::Ask, "1", "100"),
            limit("b", "b1", Side::Ask, "1", "100"),
        ]);
        let fok = Request::New {
            account: acct("a"),
            order: Order::limit(ClOrdId::new("a2"), inst(), Side::Bid, qty("2"), px("100"))
                .with_time_in_force(TimeInForce::FillOrKill),
        };
        let out = v.send(vec![fok]);
        assert!(Venue::fills(&out).is_empty());
        assert!(matches!(
            out.reports.last().unwrap(),
            Report::Cancelled {
                reason: CancelReason::FillOrKill,
                ..
            }
        ));
        assert_eq!(v.state.live_orders(), 2);
    }

    #[test]
    fn immediate_or_cancel_does_not_rest() {
        let mut v = Venue::new();
        v.send(vec![limit("m", "m1", Side::Ask, "1", "100")]);
        let ioc = Request::New {
            account: acct("t"),
            order: Order::limit(ClOrdId::new("t1"), inst(), Side::Bid, qty("3"), px("100"))
                .with_time_in_force(TimeInForce::ImmediateOrCancel),
        };
        let out = v.send(vec![ioc]);
        assert_eq!(Venue::fills(&out).len(), 2);
        assert!(matches!(
            out.reports.last().unwrap(),
            Report::Cancelled { leaves_qty, reason: CancelReason::Unfilled, .. } if *leaves_qty == qty("2")
        ));
        assert_eq!(v.state.live_orders(), 0);
    }

    #[test]
    fn rejects_bad_requests_without_failing() {
        let mut v = Venue::new();
        let other = Request::New {
            account: acct("a"),
            order: Order::limit(
                ClOrdId::new("x"),
                InstrumentId::new("arena", "ETH"),
                Side::Bid,
                qty("1"),
                px("1"),
            ),
        };
        let out = v.send(vec![
            limit("a", "1", Side::Bid, "1", "100.2"),
            limit("a", "2", Side::Bid, "0.0001", "100"),
            other,
            limit("a", "3", Side::Bid, "1", "100"),
            limit("a", "3", Side::Bid, "1", "100"),
            Request::Cancel {
                account: acct("a"),
                cl_ord_id: ClOrdId::new("nope"),
            },
            Request::Cancel {
                account: acct("b"),
                cl_ord_id: ClOrdId::new("3"),
            },
        ]);
        let reasons: Vec<_> = out
            .reports
            .iter()
            .filter_map(|r| match r {
                Report::Rejected { reason, .. } => Some(reason.clone()),
                _ => None,
            })
            .collect();
        assert_eq!(
            reasons,
            vec![
                RejectReason::OffTick,
                RejectReason::OffLot,
                RejectReason::UnknownInstrument,
                RejectReason::DuplicateClOrdId,
                RejectReason::UnknownOrder,
                RejectReason::UnknownOrder,
            ]
        );
        assert_eq!(v.state.live_orders(), 1);
    }

    #[test]
    fn cancel_removes_the_order_and_reports_leaves() {
        let mut v = Venue::new();
        v.send(vec![limit("a", "1", Side::Bid, "2", "100")]);
        let out = v.send(vec![Request::Cancel {
            account: acct("a"),
            cl_ord_id: ClOrdId::new("1"),
        }]);
        assert!(matches!(
            &out.reports[0],
            Report::Cancelled { leaves_qty, reason: CancelReason::Requested, .. } if *leaves_qty == qty("2")
        ));
        assert_eq!(v.book().best(Side::Bid), None);
        // The id is free again.
        let out = v.send(vec![limit("a", "1", Side::Bid, "1", "100")]);
        assert!(matches!(out.reports[0], Report::Accepted { .. }));
    }

    #[test]
    fn amend_down_keeps_priority_amend_up_loses_it() {
        let mut v = Venue::new();
        v.send(vec![
            limit("a", "a1", Side::Ask, "2", "100"),
            limit("b", "b1", Side::Ask, "2", "100"),
        ]);
        v.send(vec![Request::Amend {
            account: acct("a"),
            cl_ord_id: ClOrdId::new("a1"),
            qty: Some(qty("1")),
            price: None,
        }]);
        let out = v.send(vec![market("t", "t1", Side::Bid, "1")]);
        assert_eq!(Venue::fills(&out)[0].0, "a");

        v.send(vec![limit("a", "a2", Side::Ask, "1", "100")]);
        v.send(vec![Request::Amend {
            account: acct("b"),
            cl_ord_id: ClOrdId::new("b1"),
            qty: Some(qty("3")),
            price: None,
        }]);
        let out = v.send(vec![market("t", "t2", Side::Bid, "1")]);
        assert_eq!(Venue::fills(&out)[0].0, "a", "b re-queued behind a2");
    }

    #[test]
    fn amend_into_the_book_trades_and_respects_filled_qty() {
        let mut v = Venue::new();
        v.send(vec![
            limit("m", "m1", Side::Ask, "5", "101"),
            limit("a", "a1", Side::Bid, "2", "100"),
        ]);
        // Partly fill a1 against a seller at 100.
        v.send(vec![limit("s", "s1", Side::Ask, "1", "100")]);
        let out = v.send(vec![Request::Amend {
            account: acct("a"),
            cl_ord_id: ClOrdId::new("a1"),
            qty: Some(qty("1")),
            price: None,
        }]);
        assert!(matches!(
            &out.reports[0],
            Report::Rejected {
                reason: RejectReason::BadAmend,
                ..
            }
        ));
        let out = v.send(vec![Request::Amend {
            account: acct("a"),
            cl_ord_id: ClOrdId::new("a1"),
            qty: None,
            price: Some(px("101")),
        }]);
        let fills = Venue::fills(&out);
        assert_eq!(fills.len(), 2);
        assert_eq!(fills[1].2.qty, qty("1"));
        assert_eq!(fills[1].2.cum_qty, qty("2"));
        assert_eq!(fills[1].2.leaves_qty, Qty::ZERO);
        assert_eq!(v.state.live_orders(), 1); // only m1's remainder
    }

    #[test]
    fn prints_trades_and_snapshots_changed_books() {
        let mut cfg = cfg();
        let mut state = ExchangeState::default();
        let mut out = ExchangeOutput::default();
        for r in [
            limit("m", "m1", Side::Ask, "1", "100"),
            limit("m", "m2", Side::Ask, "1", "100"),
            limit("m", "m3", Side::Bid, "1", "99"),
            limit("t", "t1", Side::Bid, "1.5", "100"),
        ] {
            process(&cfg, &mut state, &r, NanoTime::ZERO, &mut out).unwrap();
        }
        let trades: Vec<_> = out
            .market
            .iter()
            .filter_map(|e| match e {
                MarketEvent::Trade(t) => Some((t.price, t.qty, t.aggressor)),
                _ => None,
            })
            .collect();
        assert_eq!(
            trades,
            vec![
                (px("100"), qty("1"), Some(Side::Bid)),
                (px("100"), qty("0.5"), Some(Side::Bid))
            ]
        );

        cfg.book_depth = 5;
        let book = state.books.get_mut(&inst()).unwrap();
        let snap = book.snapshot(cfg.book_depth, NanoTime::ZERO);
        assert_eq!(snap.asks, vec![Level::new(px("100"), qty("0.5"))]);
        assert_eq!(snap.bids, vec![Level::new(px("99"), qty("1"))]);
    }
}
