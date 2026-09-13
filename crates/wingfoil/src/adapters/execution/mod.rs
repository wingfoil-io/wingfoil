//! Execution adapter — the venue-neutral **order and fill vocabulary**, the
//! position/PnL fold that consumes it, and a simulated venue to close the loop
//! against replayed market data.
//!
//! This is the execution-side counterpart to [`market`](crate::adapters::market):
//! that module is the vocabulary venue adapters normalise market data *into*,
//! this one is the vocabulary a strategy emits orders *in*. Neither connects to
//! anything itself. Together they are what makes the swap point below possible.
//!
//! - [`Order`], [`Fill`], [`OrderType`], [`TimeInForce`], [`ClOrdId`],
//!   [`ExecId`] — the types.
//! - [`Notional`] — fixed-point money, the third member of the
//!   [`Px`]/[`Qty`] family.
//! - [`Position`] — the fold over fills, with realized and unrealized PnL.
//! - [`sim::SimVenue`] — a fill-at-touch simulated venue, behind the
//!   `execution-sim` feature.
//!
//! # The swap point is the typed order stream, above FIX
//!
//! The strategy emits [`Order`]s and consumes [`Fill`]s. Live, that stream is
//! encoded onto a venue's protocol and the fills come back decoded; in a
//! backtest, [`sim::SimVenueOps::sim_venue`] matches the same orders against a
//! replayed [`OrderBook`](crate::adapters::market::OrderBook). The strategy
//! graph is identical either way, which is the property the whole layer exists
//! to provide.
//!
//! ```text
//!                       ┌──────────────────────────────┐
//!   market data ───────▶│          strategy            │◀─── fills
//!                       └──────────────┬───────────────┘
//!                                      │ orders
//!                     ┌────────────────┴────────────────┐
//!               RunMode::RealTime              RunMode::HistoricalFrom
//!                     │                                 │
//!             ┌───────▼────────┐               ┌────────▼────────┐
//!             │  order codec   │               │    SimVenue     │
//!             │  ↕ FIX / venue │               │  (+ book, fees) │
//!             └───────┬────────┘               └────────┬────────┘
//!                     │ fills                           │ fills
//!                     └────────────────┬────────────────┘
//!                                      │
//!                               position / PnL fold
//! ```
//!
//! The codec half of that picture is **not built yet** — see
//! `docs/planning/proposals/trading-stack.md` §9, where it is gate P1. What is
//! here is P0: the types, the fold, the simulator, and the test that proves the
//! loop closes.
//!
//! # Layering
//!
//! Following the [`market`](crate::adapters::market) /
//! [`statistics`](crate::adapters::statistics) pattern, nothing here is in the
//! [`prelude`](crate::prelude). Bring in what you need explicitly:
//!
//! - **Fold** — `use wingfoil::adapters::execution::PositionOps;`
//! - **Simulator** — `use wingfoil::adapters::execution::sim::SimVenueOps;`
//!
//! # `Order` is designed from the FIX side
//!
//! A simulator can consume any shape; FIX cannot express any shape. So the
//! fields are the ones a `NewOrderSingle` and an `ExecutionReport` actually
//! need — [`ClOrdId`] (tag 11), [`Order::transact_time`] (tag 60),
//! [`Fill::cum_qty`] (tag 14) and [`Fill::leaves_qty`] (tag 151) are on the
//! types because the codec in gate P1 will require them, not because the
//! simulator wanted them. Designed the other way round, the codec would fight
//! the type forever.
//!
//! Two conventions carry straight over from
//! [`market`](crate::adapters::market) and are not re-litigated here: prices
//! and quantities are [`Px`]/[`Qty`], never `f64`; and both timestamps are
//! recorded, with [`recv_time`](Fill::recv_time) set from
//! [`Ctx::time`](crate::op::Ctx::time) so a recorded session stays replayable.
//!
//! # The shape is [`Burst`] on both edges
//!
//! Not an incidental detail. **Fills are intrinsically burst-shaped**: one
//! order crossing several book levels produces several fills at the same
//! instant, so even a single scalar [`Order`] in yields a `Burst<Fill>` out. A
//! 1:1 `Order → Fill` signature would be wrong from the first line.
//!
//! The order edge is a genuine choice rather than a forced one, and it is
//! settled the same way: **orders travel as `Burst<Order>`**. Two reasons. It
//! matches the convention [`market`](crate::adapters::market) already sets for
//! every edge in this layer, and it defuses the [`TimeQueue`] dedup question —
//! two identical orders in one instant live *inside one value*, where dedup
//! cannot split them. (Order identity closes the remainder: two orders with
//! distinct [`ClOrdId`]s are never equal values.) Both halves are pinned in
//! `tests/execution_loop.rs`.
//!
//! # Deviations
//!
//! There is no legacy `execution` adapter, so there is no parity oracle and
//! nothing to deviate *from*. Three departures from the `/new-adapter`
//! conventions are worth naming, because all three are deliberate:
//!
//! 1. **[`Notional`] is built from [`market`](crate::adapters::market)'s
//!    `fixed_point!` macro**, which that module now exposes `pub(crate)`
//!    alongside its `parse_fixed`/`fmt_fixed` helpers. No other adapter reaches
//!    into another's internals. The alternative was a second hand-rolled
//!    fixed-point implementation with its own parse and format rules, which is
//!    the kind of duplication the `Sym` rule exists to prevent — one decimal
//!    representation in the tree, not two.
//! 2. **[`Order`] and [`Fill`] hand-write `Default`** rather than deriving it,
//!    because [`Side`] deliberately has no
//!    default and `market` is right not to claim one. The engine still needs
//!    *something* in a stream's pre-first-tick value slot, so these pick
//!    [`Side::Bid`] and document that the
//!    value is not a meaningful order.
//! 3. **There is no source and no sink.** Like
//!    [`market`](crate::adapters::market) and
//!    `augurs` this adapter connects to nothing; it
//!    is transform ops and a vocabulary. Venue *execution* adapters stay out of
//!    tree, per the same reasoning that keeps venue market data adapters out.
//!
//! And one thing this module deliberately does **not** have: a cancel or
//! amend. P0 needs neither to close the loop, and inventing the vocabulary
//! before the order-state question (`docs/planning/proposals/trading-stack.md`
//! §12) is answered would settle it by accident.
//!
//! [`TimeQueue`]: crate::runtime::time_queue::TimeQueue
//! [`Px`]: crate::adapters::market::Px
//! [`Qty`]: crate::adapters::market::Qty
//! [`Burst`]: crate::Burst
//!
//! # Example
//!
//! Folding fills into a position, with no graph involved — which is also how a
//! venue adapter's own tests should exercise its decoding:
//!
//! ```
//! use wingfoil::NanoTime;
//! use wingfoil::adapters::execution::{ClOrdId, ExecId, Fill, Notional, Position};
//! use wingfoil::adapters::market::{InstrumentId, Px, Qty, Side};
//!
//! let inst = InstrumentId::new("example", "BTC-USD");
//! let mut position = Position::new(inst.clone());
//!
//! let bought = Fill {
//!     exec_id: ExecId::new("e1"),
//!     cl_ord_id: ClOrdId::new("o1"),
//!     instrument: inst.clone(),
//!     side: Side::Bid,
//!     qty: Qty::parse("2")?,
//!     price: Px::parse("100")?,
//!     cum_qty: Qty::parse("2")?,
//!     leaves_qty: Qty::ZERO,
//!     fee: Notional::ZERO,
//!     venue_time: None,
//!     recv_time: NanoTime::ZERO,
//! };
//! position.apply(&bought)?;
//! assert_eq!(position.net_qty(), Qty::parse("2")?);
//! assert_eq!(position.avg_price(), Some(Px::parse("100")?));
//!
//! // Marked up ten points, still open: the gain is unrealized.
//! assert_eq!(position.realized_pnl(), Notional::ZERO);
//! assert_eq!(position.unrealized_pnl(Px::parse("110")?)?, Notional::parse("20")?);
//! # Ok::<(), anyhow::Error>(())
//! ```

use std::fmt;

use anyhow::{Result, bail};

use crate::adapters::common::Sym;
use crate::adapters::market::{InstrumentId, Px, Qty, SCALE, Side, fixed_point};
// The macro body names these unqualified at the expansion site.
use crate::NanoTime;
use crate::adapters::market::{fmt_fixed, parse_fixed};

mod position;
pub use position::{Position, PositionBurstOp, PositionOp, PositionOps};

#[cfg(feature = "execution-sim")]
pub mod sim;

fixed_point!(Notional, "money amount");

// -------------------------------------------------------------------------
// Identity.
// -------------------------------------------------------------------------

macro_rules! sym_id {
    ($name:ident, $what:literal, $tag:literal) => {
        #[doc = concat!("A ", $what, " — FIX tag ", $tag, ".")]
        ///
        /// A string, because that is what the protocol carries, held as a
        /// [`Sym`] so cloning it into every message costs an atomic increment
        /// rather than an allocation. Equality is by content.
        ///
        /// Whether the execution path eventually wants a narrower handle than
        /// an interned string is an open question, recorded in
        /// `docs/planning/proposals/trading-stack.md` §12; it is not one P0
        /// needs to answer.
        #[derive(Clone, Debug, Default, PartialEq, Eq, PartialOrd, Ord, Hash)]
        pub struct $name(pub Sym);

        impl $name {
            #[doc = concat!("Build a ", $what, ".")]
            pub fn new(id: impl AsRef<str>) -> Self {
                Self(Sym::new(id))
            }

            /// The underlying string.
            pub fn as_str(&self) -> &str {
                self.0.as_str()
            }
        }

        impl fmt::Display for $name {
            fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                write!(f, "{}", self.0)
            }
        }
    };
}

sym_id!(ClOrdId, "client order id", "11");
sym_id!(ExecId, "venue execution id", "17");

// -------------------------------------------------------------------------
// Order.
// -------------------------------------------------------------------------

/// How an order is priced — FIX `OrdType`, tag 40.
///
/// Only the two types every venue supports. A venue-specific type (stop,
/// pegged, iceberg) is not a parameterisation of these and does not belong in
/// the lowest-common-denominator vocabulary; see
/// `docs/planning/proposals/trading-stack.md` §4.3.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum OrderType {
    /// Execute against whatever is resting, at whatever price — tag 40 = `1`.
    Market,
    /// Execute only at [`Order::price`] or better — tag 40 = `2`.
    #[default]
    Limit,
}

/// How long an order stays working — FIX `TimeInForce`, tag 59.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum TimeInForce {
    /// Works until the end of the session — tag 59 = `0`.
    #[default]
    Day,
    /// Works until explicitly cancelled — tag 59 = `1`.
    GoodTillCancel,
    /// Fills what it can immediately; the remainder is cancelled — tag 59 = `3`.
    ImmediateOrCancel,
    /// Fills in full immediately or not at all — tag 59 = `4`.
    FillOrKill,
}

impl TimeInForce {
    /// Whether an unfilled remainder rests on the book rather than being
    /// cancelled.
    pub const fn rests(self) -> bool {
        matches!(self, TimeInForce::Day | TimeInForce::GoodTillCancel)
    }
}

/// An order as sent to a venue — the strategy's output edge.
///
/// The `Default` impl exists only because the engine requires every stream's
/// value type to be `Default` for its pre-first-tick value slot. It is not a
/// meaningful order — it has no id, no instrument and zero quantity, and
/// [`validate`](Order::validate) rejects it.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Order {
    /// The client's own id for this order — FIX tag 11. Unique per order.
    pub cl_ord_id: ClOrdId,
    /// What is being traded.
    pub instrument: InstrumentId,
    /// Buy ([`Side::Bid`]) or sell ([`Side::Ask`]).
    pub side: Side,
    /// The order's total quantity — FIX tag 38. Always positive; direction
    /// lives in [`side`](Order::side).
    pub qty: Qty,
    /// The limit price — FIX tag 44. `None` for a [`OrderType::Market`] order,
    /// and required for a [`OrderType::Limit`] one.
    pub price: Option<Px>,
    /// How the order is priced.
    pub order_type: OrderType,
    /// How long it works.
    pub time_in_force: TimeInForce,
    /// Engine time at which the strategy emitted it — FIX tag 60. Set from
    /// [`Ctx::time`](crate::op::Ctx::time), never from
    /// [`NanoTime::now`](crate::NanoTime::now), so a recorded session replays.
    pub transact_time: NanoTime,
}

/// Hand-written rather than derived because [`Side`] deliberately has no
/// default — there is no such thing as a default side, and `market` is right
/// not to claim one. The value slot still needs *something*.
impl Default for Order {
    fn default() -> Self {
        Self {
            cl_ord_id: ClOrdId::default(),
            instrument: InstrumentId::default(),
            side: Side::Bid,
            qty: Qty::ZERO,
            price: None,
            order_type: OrderType::default(),
            time_in_force: TimeInForce::default(),
            transact_time: NanoTime::ZERO,
        }
    }
}

impl Order {
    /// A limit order, `Day` by default.
    pub fn limit(
        cl_ord_id: ClOrdId,
        instrument: InstrumentId,
        side: Side,
        qty: Qty,
        price: Px,
    ) -> Self {
        Self {
            cl_ord_id,
            instrument,
            side,
            qty,
            price: Some(price),
            order_type: OrderType::Limit,
            time_in_force: TimeInForce::Day,
            transact_time: NanoTime::ZERO,
        }
    }

    /// A market order, `ImmediateOrCancel` by default — a market order that
    /// rests is a contradiction, and the remainder has to go somewhere.
    pub fn market(cl_ord_id: ClOrdId, instrument: InstrumentId, side: Side, qty: Qty) -> Self {
        Self {
            cl_ord_id,
            instrument,
            side,
            qty,
            price: None,
            order_type: OrderType::Market,
            time_in_force: TimeInForce::ImmediateOrCancel,
            transact_time: NanoTime::ZERO,
        }
    }

    /// Set the time in force.
    #[must_use]
    pub fn with_time_in_force(mut self, tif: TimeInForce) -> Self {
        self.time_in_force = tif;
        self
    }

    /// Stamp the order with the engine time it was emitted at.
    #[must_use]
    pub fn with_transact_time(mut self, time: NanoTime) -> Self {
        self.transact_time = time;
        self
    }

    /// Validate the combination of fields a venue would reject.
    ///
    /// # Errors
    ///
    /// A non-positive quantity, a limit order with no price, or a market order
    /// carrying one.
    pub fn validate(&self) -> Result<()> {
        if self.qty.raw() <= 0 {
            bail!(
                "order {} has quantity {}; an order's quantity must be positive \
                 (direction is carried by `side`)",
                self.cl_ord_id,
                self.qty
            );
        }
        match (self.order_type, self.price) {
            (OrderType::Limit, None) => {
                bail!("order {} is a limit order with no price", self.cl_ord_id)
            }
            (OrderType::Market, Some(price)) => bail!(
                "order {} is a market order carrying a limit price of {price}",
                self.cl_ord_id
            ),
            _ => Ok(()),
        }
    }
}

// -------------------------------------------------------------------------
// Fill.
// -------------------------------------------------------------------------

/// One execution against an [`Order`] — the strategy's input edge.
///
/// Maps onto the fill-bearing subset of a FIX `ExecutionReport`. Partial fills
/// are the normal case, which is why [`cum_qty`](Fill::cum_qty) and
/// [`leaves_qty`](Fill::leaves_qty) ride on every one: a consumer that wants
/// order state should not have to re-derive it by summing.
///
/// The `Default` impl exists only for the engine's value slot, as for
/// [`Order`], and is hand-written for the same reason.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Fill {
    /// The venue's id for this execution — FIX tag 17. Unique per fill.
    pub exec_id: ExecId,
    /// The order this fill belongs to — FIX tag 11.
    pub cl_ord_id: ClOrdId,
    /// What was traded.
    pub instrument: InstrumentId,
    /// The side of the *order*, not of the resting liquidity it hit.
    pub side: Side,
    /// This execution's quantity — FIX `LastQty`, tag 32. Always positive.
    pub qty: Qty,
    /// This execution's price — FIX `LastPx`, tag 31.
    pub price: Px,
    /// Total quantity filled on the order so far, this fill included — FIX
    /// tag 14.
    pub cum_qty: Qty,
    /// Quantity still working on the order after this fill — FIX tag 151.
    /// Zero once the order is done, whether filled or cancelled.
    pub leaves_qty: Qty,
    /// The fee charged for this execution, positive for a cost and negative
    /// for a rebate.
    ///
    /// **A simulator that reports zero here is lying in a specific and
    /// flattering direction** — a maker/taker schedule flips the sign of most
    /// passive strategies. P0's [`sim`] does exactly that, deliberately and
    /// visibly; a fee model is gate P2.
    pub fee: Notional,
    /// The venue's own clock, where it sends one. Never trusted for ordering
    /// across venues.
    pub venue_time: Option<NanoTime>,
    /// Engine time at which the fill reached the graph — set from
    /// [`Ctx::time`](crate::op::Ctx::time), which is what keeps a recorded
    /// session replayable.
    pub recv_time: NanoTime,
}

impl Default for Fill {
    fn default() -> Self {
        Self {
            exec_id: ExecId::default(),
            cl_ord_id: ClOrdId::default(),
            instrument: InstrumentId::default(),
            side: Side::Bid,
            qty: Qty::ZERO,
            price: Px::ZERO,
            cum_qty: Qty::ZERO,
            leaves_qty: Qty::ZERO,
            fee: Notional::ZERO,
            venue_time: None,
            recv_time: NanoTime::ZERO,
        }
    }
}

impl Fill {
    /// The signed quantity this fill moves the position by: positive for a
    /// buy, negative for a sell.
    pub const fn signed_qty(&self) -> Qty {
        match self.side {
            Side::Bid => self.qty,
            Side::Ask => Qty::from_raw(-self.qty.raw()),
        }
    }

    /// This execution's notional value — price × quantity, unsigned.
    ///
    /// # Errors
    ///
    /// If the product overflows the fixed-point range.
    pub fn notional(&self) -> Result<Notional> {
        notional_of(self.price, self.qty)
    }
}

// -------------------------------------------------------------------------
// Fixed-point arithmetic helpers.
// -------------------------------------------------------------------------

/// `price × quantity`, rescaled from the doubled product back to one scale
/// factor.
///
/// Every money calculation in this module funnels through here or
/// [`scaled_mul`], so the one place a scale can be got wrong is this one.
pub(crate) fn notional_of(price: Px, qty: Qty) -> Result<Notional> {
    scaled_mul(price.raw(), qty.raw()).map(Notional::from_raw)
}

/// Multiply two [`SCALE`](crate::adapters::market::SCALE)-scaled integers back
/// down to one scale factor.
///
/// Truncates towards zero on the division, so a product finer than
/// [`DECIMALS`](crate::adapters::market::DECIMALS) loses its tail rather than
/// rounding — the same trade
/// [`Px::parse`](crate::adapters::market::Px::parse) makes at the parse
/// boundary, and in the same direction: no silent growth.
pub(crate) fn scaled_mul(a: i128, b: i128) -> Result<i128> {
    let product = a
        .checked_mul(b)
        .ok_or_else(|| anyhow::anyhow!("fixed-point multiplication of {a} and {b} overflows"))?;
    Ok(product / SCALE)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn notional_scales_price_by_quantity() {
        let value = notional_of(Px::parse("100.5").unwrap(), Qty::parse("3").unwrap()).unwrap();
        assert_eq!(value, Notional::parse("301.5").unwrap());
    }

    #[test]
    fn notional_truncates_below_the_representable_scale() {
        // 1e-9 × 0.5 = 5e-10, which nine decimal places cannot hold.
        let value = notional_of(Px::from_raw(1), Qty::parse("0.5").unwrap()).unwrap();
        assert_eq!(value, Notional::ZERO);
    }

    #[test]
    fn notional_reports_overflow_rather_than_wrapping() {
        let huge = Px::from_raw(i128::MAX / 2);
        let error = notional_of(huge, Qty::parse("1000").unwrap()).unwrap_err();
        assert!(error.to_string().contains("overflows"), "{error}");
    }

    #[test]
    fn signed_qty_carries_the_side() {
        let mut fill = Fill {
            qty: Qty::parse("2").unwrap(),
            side: Side::Bid,
            ..Fill::default()
        };
        assert_eq!(fill.signed_qty(), Qty::parse("2").unwrap());
        fill.side = Side::Ask;
        assert_eq!(fill.signed_qty(), Qty::parse("-2").unwrap());
    }

    #[test]
    fn order_validation_rejects_what_a_venue_would() {
        let inst = InstrumentId::new("test", "BTC-USD");
        let good = Order::limit(
            ClOrdId::new("o1"),
            inst.clone(),
            Side::Bid,
            Qty::parse("1").unwrap(),
            Px::parse("100").unwrap(),
        );
        assert!(good.validate().is_ok());

        let mut no_price = good.clone();
        no_price.price = None;
        assert!(
            no_price
                .validate()
                .unwrap_err()
                .to_string()
                .contains("no price")
        );

        let mut zero_qty = good.clone();
        zero_qty.qty = Qty::ZERO;
        assert!(
            zero_qty
                .validate()
                .unwrap_err()
                .to_string()
                .contains("must be positive")
        );

        let mut priced_market = Order::market(
            ClOrdId::new("o2"),
            inst,
            Side::Ask,
            Qty::parse("1").unwrap(),
        );
        priced_market.price = Some(Px::parse("100").unwrap());
        assert!(
            priced_market
                .validate()
                .unwrap_err()
                .to_string()
                .contains("limit price")
        );
    }

    #[test]
    fn time_in_force_says_which_orders_rest() {
        assert!(TimeInForce::Day.rests());
        assert!(TimeInForce::GoodTillCancel.rests());
        assert!(!TimeInForce::ImmediateOrCancel.rests());
        assert!(!TimeInForce::FillOrKill.rests());
    }

    /// The `Default` of a type a caller constructs by struct update is a
    /// compatibility surface: changing one silently changes behaviour at every
    /// default call site. Pin them.
    #[test]
    fn defaults_are_the_conservative_ones() {
        assert_eq!(OrderType::default(), OrderType::Limit);
        assert_eq!(TimeInForce::default(), TimeInForce::Day);
        assert_eq!(Order::default().transact_time, NanoTime::ZERO);
        assert_eq!(Fill::default().fee, Notional::ZERO);
    }
}
