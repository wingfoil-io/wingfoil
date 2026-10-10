//! The position fold: what is held, what it cost, and what it is worth.
//!
//! A fold over fills, never a fan-in join, and the reason this is one keyed
//! map rather than a node per instrument. It is what a quoter reads to know
//! its inventory, what exposure is computed over, and what PnL attribution
//! splits.
//!
//! # What an instrument is worth is the caller's to say
//!
//! [`Book`] is generic over the instrument and reads nothing of it but its
//! identity. How an instrument's PnL runs — its [`Measure`], or `None` where
//! the caller cannot value it — is a closure handed to [`Book::new`], and
//! which currency it settles in is a predicate handed to [`Book::total`].
//! Both are facts about the contract, which the caller's instrument type
//! knows and this module does not.
//!
//! # An inverse contract's PnL is not linear in the price
//!
//! This is the arithmetic the fold exists for. An inverse future is sized in
//! the *quote* currency and settles in the *base*: long `N` dollars of
//! notional entered at `P` and marked at `M` is worth `N × (1/P − 1/M)` of
//! the base. Straight-line PnL in `P`
//! is wrong, and wrong in the direction that matters — it understates the
//! loss on the way down, which is the side a short gamma book is exposed on.
//!
//! A linear contract — a spot position, a linear future, an inverse
//! *option*, sized in the base with its premium quoted in the base — is linear:
//! `q × (m − c)`, times the contract's multiplier where one unit is not
//! worth one price unit (an index future at $50 a point).
//!
//! One fold has to do both, so it holds neither formula. It holds a
//! [`Measure`]: a per-unit number, in the settlement currency, chosen so that
//! PnL is always
//!
//! ```text
//! net × measure(mark) − cost
//! ```
//!
//! For a linear contract the measure is the price itself, times the
//! multiplier where there is one ([`Measure::Scaled`]). For an inverse one it
//! is the **negated** reciprocal of the price, `−1/P`, and the negation is
//! what makes the formula hold — a long inverse future gains as the price
//! rises, while the base its notional is worth falls. Everything else here is
//! that one line: [`Position::cost`] is the measure the position is held at,
//! and a fill that reduces the position turns the difference into
//! [`realised`](Position::realised).
//!
//! # What it will not guess at
//!
//! **A position is always tracked; its value sometimes is not.** An
//! instrument the caller gives no [`Measure`] is not valued, so
//! [`Position::pnl`] answers `None` for it. The *net* is still folded, because
//! the number a strategy must never be wrong about is how much of something it
//! is holding: a quoter that thinks it is flat keeps selling. The number it
//! can afford to be told it does not have is what that is worth.
//!
//! **Currencies are not added together.** A position's PnL is in its
//! instrument's settlement currency, implied by the instrument rather than
//! carried as a field — the same decision [`Fill::fee`] takes, and for the
//! same reason: a separate field could only ever disagree with the
//! instrument. [`Book::total`] therefore totals one currency at a time and
//! says which.
//!
//! **A total that omits an open position is not a total.** Where a position
//! has a net and no usable mark, [`Book::total`] answers `None` rather than
//! quietly summing the rest. A flat position needs no mark and does not
//! withhold one.
//!
//! [`Fill::fee`]: crate::adapters::execution::order::Fill::fee

use std::collections::BTreeMap;
use std::fmt;
use std::rc::Rc;

use crate::NanoTime;
use crate::adapters::execution::exec_id::ExecId;
use crate::adapters::execution::order::{ClientOrderId, Fill, Instrument, Liquidity};
use crate::adapters::market::{Px, Qty, SCALE, Side, mul_div};

/// The per-unit number, in the settlement currency, that makes an
/// instrument's PnL linear.
///
/// Chosen so that the PnL of holding `net` units is
/// `net × (measure(now) − measure(entry))` whichever kind of contract it is.
/// See the module docs: that one identity is the whole component.
///
/// A contract whose unit is not worth one price unit is
/// [`Scaled`](Self::Scaled), not a formula of its own: an index future, a
/// bond quoted in points of par, an FX lot are each "the price, times what
/// one unit is worth", and the fold needs to know nothing else about them.
/// [`Linear`](Self::Linear) stays the unit case rather than becoming
/// `Scaled { multiplier: 1 }` because it is what every contract sized in
/// what it is priced in says — a base-settled option, a spot position — and a
/// multiplier of one spelled out at each of those call sites is a number a
/// reader has to check is one. [`Measure::scaled`] folds a multiplier of one
/// back into `Linear`, so the unit case has one spelling.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Measure {
    /// The measure *is* the price: a contract sized in units of what it
    /// is priced in the settlement currency of — a spot position, a linear
    /// future, an inverse option (sized and quoted in the base currency). A
    /// long gains as the price rises.
    Linear,
    /// The measure is the price times what one unit is worth per unit of
    /// price, in the settlement currency: an index future at $50 a point, a
    /// bond whose face is a hundred times its price in points, a contract of
    /// a hundred shares. [`Linear`](Self::Linear) with a multiplier; a long
    /// gains as the price rises.
    ///
    /// The multiplier must be positive. Zero would value every position at
    /// nothing, which reads as a flat PnL rather than a missing one, and a
    /// negative one would book a long's PnL as a short's. A variant's field
    /// cannot be private, so the check is where it is read:
    /// [`value`](Self::value) and the entry price answer `None` for a
    /// multiplier that is not positive, and a [`Book`] takes such a measure
    /// for none at all — the position is folded, not valued, and counted in
    /// [`Book::unvalued`]. [`Measure::scaled`] refuses one up front.
    Scaled {
        /// What one unit is worth per unit of price, in the settlement
        /// currency. Positive.
        multiplier: Qty,
    },
    /// The measure is the negated reciprocal of the price: an inverse future
    /// or perpetual, sized in the quote currency and settled in the base. A long
    /// gains as the price rises, which is as `−1/P` rises.
    Inverse,
}

impl Measure {
    /// A contract whose one unit is worth `multiplier` per unit of price, or
    /// `None` for a multiplier that is not positive — see
    /// [`Scaled`](Self::Scaled).
    ///
    /// A multiplier of exactly one is [`Linear`](Self::Linear), so the unit
    /// case has one spelling and two books valuing the same contract compare
    /// equal.
    pub fn scaled(multiplier: Qty) -> Option<Measure> {
        if multiplier <= Qty::ZERO {
            None
        } else if multiplier == Qty::from_raw(SCALE) {
            Some(Measure::Linear)
        } else {
            Some(Measure::Scaled { multiplier })
        }
    }

    /// Whether this can value anything: every measure but a
    /// [`Scaled`](Self::Scaled) one whose multiplier is not positive.
    fn is_valid(self) -> bool {
        match self {
            Measure::Scaled { multiplier } => multiplier > Qty::ZERO,
            Measure::Linear | Measure::Inverse => true,
        }
    }

    /// `qty` units valued at `price`, in the settlement currency.
    ///
    /// Exact: the multiply happens before the divide, which for the
    /// reciprocal is the difference between nine significant digits and four
    /// ([`mul_div`]). `None` for a price of zero under
    /// [`Inverse`](Self::Inverse), which is not a price; for a
    /// [`Scaled`](Self::Scaled) multiplier that is not positive, which is not
    /// a measure; and for a value too large to hold.
    ///
    /// A scaled value is two steps, `qty × multiplier` and then `× price`,
    /// because the product of three `10^9`-scaled numbers is `10^27`-scaled,
    /// where `i128` holds a value of only about `1.7 × 10^11` — a million
    /// index futures at 5,000 and fifty a point is past it. Two steps hold
    /// the intermediate at `10^18`. Contracts into units first: a multiplier is a
    /// whole number or a short binary fraction, so that step is exact and
    /// the only rounding is the price's — where `qty × price` first would
    /// round and then multiply the rounding by the multiplier.
    pub fn value(self, qty: Qty, price: Px) -> Option<Qty> {
        match self {
            Measure::Linear => mul(qty, price),
            Measure::Scaled { multiplier } => mul(units(qty, multiplier)?, price),
            Measure::Inverse => div(qty, price).map(|q| -q),
        }
    }

    /// [`value`](Self::value) read backwards: the average price `net` units
    /// costing `cost` were entered at.
    ///
    /// In one step from the two accumulated numbers, never through a
    /// per-unit measure. A per-unit inverse measure near `1 / 60_000` has
    /// four significant digits in this type, and inverting *that* puts the
    /// rounding back into the price — a $60,000 entry reads as $60,002.40.
    /// The accumulated cost has the full nine, so the division that
    /// apportions it happens once, here.
    ///
    /// `None` for a flat position, which was entered at no price, and for a
    /// [`Scaled`](Self::Scaled) multiplier that is not positive.
    fn price_from(self, net: Qty, cost: Qty) -> Option<Px> {
        match self {
            // `cost = net × price / SCALE`.
            Measure::Linear => mul_div(cost.raw(), SCALE, net.raw()).map(Px::from_raw),
            // `cost = units × price / SCALE`, the units taken as `value`
            // takes them, so the one division undoes the one multiply.
            Measure::Scaled { multiplier } => {
                mul_div(cost.raw(), SCALE, units(net, multiplier)?.raw()).map(Px::from_raw)
            }
            // `cost = −net × SCALE / price`.
            Measure::Inverse => mul_div(net.raw(), SCALE, -cost.raw()).map(Px::from_raw),
        }
    }
}

/// `qty × multiplier`: what `qty` contracts come to in units of the price.
/// `None` for a multiplier that is not positive — see [`Measure::Scaled`].
fn units(qty: Qty, multiplier: Qty) -> Option<Qty> {
    if multiplier <= Qty::ZERO {
        return None;
    }
    mul_div(qty.raw(), multiplier.raw(), SCALE).map(Qty::from_raw)
}

/// `qty × price`, as a quantity of the currency the price is in. The scale
/// cancels once: two `10^9`-scaled integers multiplied are `10^18`-scaled.
fn mul(qty: Qty, price: Px) -> Option<Qty> {
    mul_div(qty.raw(), price.raw(), SCALE).map(Qty::from_raw)
}

/// `qty / price`, as a quantity of the currency the price is quoted in terms
/// of: what an inverse contract's notional is worth. `qty × SCALE` is taken
/// before the division, so a reciprocal near `1 / 60_000` keeps nine digits.
/// `None` for a price of zero.
fn div(qty: Qty, price: Px) -> Option<Qty> {
    mul_div(qty.raw(), SCALE, price.raw()).map(Qty::from_raw)
}

/// PnL, split the way attribution wants it.
///
/// The currency is not a field. It is the instrument's settlement currency
/// for the same reason [`Fill::fee`] does not
/// carry one: a field could only ever disagree with the instrument.
///
/// [`Fill::fee`]: crate::adapters::execution::order::Fill::fee
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Pnl {
    /// What closing trades have booked, **gross of fees**. Kept gross
    /// because attribution splits fees out as their own line, and a
    /// realised number with them already in it could not be split back out.
    pub realised: Qty,
    /// What the open position is worth against a mark, over what it is
    /// carried at.
    pub unrealised: Qty,
    /// Fees paid, positive being a cost. A maker rebate is negative, which
    /// is why this is not an unsigned number.
    pub fees: Qty,
    /// Cashflows the venue moved on the position — funding, coupons,
    /// dividends, borrow — positive being money in. Its own field for the
    /// reason fees are: it is money that moved with no price behind it, and
    /// attribution wants it as its own line rather than left in the residual.
    pub cashflows: Qty,
}

impl Pnl {
    /// What it comes to: realised plus unrealised plus cashflows, less fees.
    pub fn net(&self) -> Qty {
        self.realised + self.unrealised + self.cashflows - self.fees
    }

    /// The same PnL in the quote currency, at `index`.
    ///
    /// An amount times the index, which for a book settled in the base
    /// currency is the whole conversion — nothing here has to know what it
    /// is a position *in*. `None` for an index that
    /// does not multiply.
    pub fn in_quote(&self, index: Px) -> Option<Pnl> {
        Some(Pnl {
            realised: mul(self.realised, index)?,
            unrealised: mul(self.unrealised, index)?,
            fees: mul(self.fees, index)?,
            cashflows: mul(self.cashflows, index)?,
        })
    }

    /// `self + other`, which is only a number within one currency — see the
    /// module docs.
    fn plus(self, other: Pnl) -> Pnl {
        Pnl {
            realised: self.realised + other.realised,
            unrealised: self.unrealised + other.unrealised,
            fees: self.fees + other.fees,
            cashflows: self.cashflows + other.cashflows,
        }
    }
}

/// What is held in one instrument, what it is carried at, and what it has
/// booked.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Position<I> {
    /// Which instrument.
    pub instrument: I,
    /// How its PnL runs, or `None` where the caller could not say. The net
    /// below is folded either way.
    pub measure: Option<Measure>,
    net: Qty,
    cost: Qty,
    realised: Qty,
    fees: Qty,
    cashflows: Qty,
}

impl<I: Copy> Position<I> {
    /// A flat position in `instrument`, valued by `measure`.
    pub const fn flat(instrument: I, measure: Option<Measure>) -> Position<I> {
        Position {
            instrument,
            measure,
            net: Qty::ZERO,
            cost: Qty::ZERO,
            realised: Qty::ZERO,
            fees: Qty::ZERO,
            cashflows: Qty::ZERO,
        }
    }

    /// A position as a previous process left it: the net
    /// and every accumulated line, exactly as [`Position`]'s accessors read
    /// them out.
    ///
    /// The one constructor besides [`flat`](Self::flat), and the reason the
    /// fields stay private: a position assembled from parts is either what a
    /// fold left or a fabrication, and this is the one place that says which.
    /// It is what carries an entry price across a restart — the cost — so
    /// the book's PnL and attribution continue from what was paid rather than
    /// from a re-base at the mark, which is all the venue's net alone could
    /// give.
    ///
    /// `measure` is carried rather than re-derived: a position a re-base
    /// could not value holds `None` whatever its instrument, and restoring it
    /// as valued would give it a cost of zero it never had.
    #[allow(clippy::too_many_arguments)]
    pub const fn restore(
        instrument: I,
        measure: Option<Measure>,
        net: Qty,
        cost: Qty,
        realised: Qty,
        fees: Qty,
        cashflows: Qty,
    ) -> Position<I> {
        Position {
            instrument,
            measure,
            net,
            cost,
            realised,
            fees,
            cashflows,
        }
    }

    /// The signed position: positive long, negative short, in the venue's own
    /// size unit for the instrument.
    ///
    /// Always right, whatever [`measure`](Self::measure) says. This is the
    /// number a strategy must never be wrong about.
    pub const fn net(&self) -> Qty {
        self.net
    }

    /// Whether nothing is held.
    pub fn is_flat(&self) -> bool {
        self.net == Qty::ZERO
    }

    /// What the position is carried at, in the settlement currency, in the
    /// measure that makes PnL linear. See the module docs: it is not the cash that changed
    /// hands, and for an inverse future it is negative where the position is
    /// long.
    pub const fn cost(&self) -> Qty {
        self.cost
    }

    /// What closing trades have booked, gross of fees.
    pub const fn realised(&self) -> Qty {
        self.realised
    }

    /// Fees paid on this instrument, positive being a cost.
    pub const fn fees(&self) -> Qty {
        self.fees
    }

    /// Cashflows booked on this instrument, positive being money in.
    pub const fn cashflows(&self) -> Qty {
        self.cashflows
    }

    /// What the position has booked whatever it is marked at: realised plus
    /// cashflows, less fees. The whole of a flat position's value.
    pub fn booked(&self) -> Qty {
        self.realised + self.cashflows - self.fees
    }

    /// The average price the open position was entered at, or `None` when it
    /// is flat or unmeasurable.
    ///
    /// Read back out of the cost rather than accumulated separately, so it
    /// cannot drift from the number the PnL is actually computed against.
    pub fn entry(&self) -> Option<Px> {
        self.measure?.price_from(self.net, self.cost)
    }

    /// What the open position is worth against `mark`, over what it is
    /// carried at.
    ///
    /// `None` where the instrument has no [`Measure`] or one that cannot
    /// value anything, or where the mark cannot be carried — a zero price on
    /// an inverse contract. A flat position is worth zero and says so rather
    /// than refusing.
    pub fn unrealised(&self, mark: Px) -> Option<Qty> {
        let measure = self.measure.filter(|measure| measure.is_valid())?;
        if self.net == Qty::ZERO {
            return Some(Qty::ZERO);
        }
        Some(measure.value(self.net, mark)? - self.cost)
    }

    /// The whole picture against `mark`.
    ///
    /// `None` for an instrument this fold cannot value; the position itself
    /// is still folded and [`net`](Self::net) still answers.
    pub fn pnl(&self, mark: Px) -> Option<Pnl> {
        Some(Pnl {
            realised: self.realised,
            unrealised: self.unrealised(mark)?,
            fees: self.fees,
            cashflows: self.cashflows,
        })
    }

    /// Apply one execution. Returns whether it could be valued.
    ///
    /// The net is folded either way — see the module docs — so `false` means
    /// "the position moved and the money did not", which is what
    /// [`Book::unvalued`] counts.
    fn apply(&mut self, fill: &Fill<I>) -> bool {
        // A fee is charged whether or not the trade can be valued, and it is
        // charged on the fill's own currency, which is the instrument's.
        self.fees = self.fees + fill.fee;

        let signed = match fill.side {
            Side::Bid => fill.qty,
            Side::Ask => -fill.qty,
        };
        let net_before = self.net;
        self.net = self.net + signed;

        let Some(measure) = self.measure else {
            return false;
        };
        let Some(delta) = measure.value(signed, fill.price) else {
            // The position has already moved; the cost cannot follow it, so
            // it is no longer a cost for this net and must not be used as
            // one. Dropping the whole position's valuation is the honest
            // reading, and it is loud.
            self.measure = None;
            self.cost = Qty::ZERO;
            return false;
        };

        // Opening — including from flat, and including the side it was
        // already on — is the whole of it: the cost takes the fill and
        // nothing is booked.
        let closing = net_before.raw().signum() * signed.raw().signum() < 0;
        if !closing {
            self.cost = self.cost + delta;
            return true;
        }

        // Closing. `released` is the cost the closed units were held at and
        // `from_fill` is the part of this fill that closed them; what they
        // come to, negated, is the amount booked. The module docs derive it —
        // the sign falls out of the two being on opposite sides.
        let closed = signed.abs().min(net_before.abs());
        let apportion =
            |whole: Qty, of: Qty| mul_div(whole.raw(), closed.raw(), of.raw()).map(Qty::from_raw);
        let (Some(released), Some(from_fill)) = (
            apportion(self.cost, net_before.abs()),
            apportion(delta, signed.abs()),
        ) else {
            // Neither divisor can be zero here — both sides of a close are
            // non-zero by construction — so this is an overflow, and a
            // realised number that could not be computed is not booked as a
            // zero that would read as a flat trade.
            self.measure = None;
            self.cost = Qty::ZERO;
            return false;
        };
        self.realised = self.realised - (released + from_fill);
        // What is left of the old cost, plus whatever of this fill flipped
        // into a new position — zero unless it went through flat.
        self.cost = self.cost - released + delta - from_fill;
        if self.net == Qty::ZERO {
            // Exactly flat: no rounding left behind to be read as a cost on
            // nothing.
            self.cost = Qty::ZERO;
        }
        true
    }
}

/// Every position, keyed on the instrument.
///
/// One fold over one map, like the OMS and for the same reason: a slot pool
/// would be a capacity choice nobody has to make.
///
/// No `Default`: a book that values nothing would read every position as
/// unvalued without anyone having said so. The caller names its valuation.
///
/// Keyed in an ordered map, so every walk of it — [`iter`](Self::iter),
/// [`positions`](Self::positions) — comes out in the instrument's order: a
/// sequence built from the book is the same on a replay as on the run it
/// replays, with no sort to remember.
#[derive(Clone)]
pub struct Book<I> {
    positions: BTreeMap<I, Position<I>>,
    unvalued: u64,
    /// A closure rather than a `fn`, so a valuation read from a registry —
    /// contract specs loaded from a file, a map of symbol to measure — can
    /// be captured. Shared, so a cloned book values as the original does
    /// without the closure having to be `Clone`; an `Rc` because a book
    /// lives on the graph thread, which is not `Send` either.
    measure: Rc<dyn Fn(&I) -> Option<Measure>>,
}

/// By hand, because the valuation is a closure and has nothing to print.
impl<I: fmt::Debug> fmt::Debug for Book<I> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Book")
            .field("positions", &self.positions)
            .field("unvalued", &self.unvalued)
            .field("measure", &format_args!("<closure>"))
            .finish()
    }
}

impl<I: Instrument> Book<I> {
    /// An empty book, valuing each instrument by `measure` — `None` for one
    /// the caller cannot value, whose net is still folded.
    pub fn new(measure: impl Fn(&I) -> Option<Measure> + 'static) -> Book<I> {
        Book {
            positions: BTreeMap::new(),
            unvalued: 0,
            measure: Rc::new(measure),
        }
    }

    /// A book as a previous process left it: `positions` as
    /// [`positions`](Self::positions) listed them, and the
    /// [`unvalued`](Self::unvalued) count carried on rather than reset.
    ///
    /// A later entry for an instrument replaces an earlier one; a list this
    /// crate wrote has one each.
    ///
    /// Each position keeps the measure it was saved with; `measure` values
    /// instruments the book first sees after the restore.
    pub fn restore(
        measure: impl Fn(&I) -> Option<Measure> + 'static,
        positions: impl IntoIterator<Item = Position<I>>,
        unvalued: u64,
    ) -> Book<I> {
        Book {
            positions: positions
                .into_iter()
                .map(|position| (position.instrument, position))
                .collect(),
            unvalued,
            measure: Rc::new(measure),
        }
    }

    /// A flat position in `instrument`, valued as this book values it.
    ///
    /// A measure that cannot value anything — a [`Measure::Scaled`] whose
    /// multiplier is not positive — is taken for no measure, so the position
    /// is folded and counted as unvalued rather than valued at a multiplier
    /// of zero.
    fn flat(&self, instrument: I) -> Position<I> {
        Position::flat(
            instrument,
            (self.measure)(&instrument).filter(|measure| measure.is_valid()),
        )
    }

    /// Apply a burst of fills, in order.
    pub fn apply(&mut self, fills: &[Fill<I>]) {
        for fill in fills {
            let flat = self.flat(fill.instrument);
            let position = self.positions.entry(fill.instrument).or_insert(flat);
            if !position.apply(fill) {
                self.unvalued += 1;
            }
        }
    }

    /// Book a cashflow the venue moved on `instrument` — funding, a coupon,
    /// a dividend, a borrow fee — positive being money in.
    ///
    /// The net and the cost are untouched — nothing traded. Never a future's
    /// variation margin, which the mark already holds
    /// ([`Cashflow`](crate::adapters::execution::edge::Cashflow)).
    /// A payment on an instrument the book has never held is still booked,
    /// on a flat position: the money moved whether or not this fold saw the
    /// position that earned it, which is the case of a restart onto a book
    /// the account already held, and dropping it would put it in
    /// attribution's residual.
    pub fn cashflow(&mut self, instrument: I, amount: Qty) {
        let flat = self.flat(instrument);
        let position = self.positions.entry(instrument).or_insert(flat);
        position.cashflows = position.cashflows + amount;
    }

    /// The signed position in one instrument — zero where there is none.
    ///
    /// This is what a quoter reads for its inventory, which is why it
    /// answers a number rather than an `Option`: a contract nobody has traded
    /// is flat, and "flat" and "absent" are the same fact about it.
    pub fn net(&self, instrument: &I) -> Qty {
        self.positions
            .get(instrument)
            .map_or(Qty::ZERO, Position::net)
    }

    /// One position, if the book has ever traded the instrument.
    pub fn position(&self, instrument: &I) -> Option<Position<I>> {
        self.positions.get(instrument).copied()
    }

    /// Every position, in the instrument's order.
    ///
    /// The same order as [`positions`](Self::positions), without the copy:
    /// a sum or a sequence can be built from it alike, and either is the
    /// same on a replay as on the run it replays.
    pub fn iter(&self) -> impl Iterator<Item = &Position<I>> {
        self.positions.values()
    }

    /// Every position, in the instrument's order, as owned values.
    ///
    /// The order is the map's, which is ordered: anything that builds a
    /// *sequence* out of this — a log line, a risk report, a burst, a
    /// snapshot for [`restore`](Self::restore) — is the same between a run
    /// and its replay.
    pub fn positions(&self) -> Vec<Position<I>> {
        self.positions.values().copied().collect()
    }

    /// How many instruments it is holding a position in, flat ones included.
    pub fn len(&self) -> usize {
        self.positions.len()
    }

    /// Whether it has seen nothing at all.
    pub fn is_empty(&self) -> bool {
        self.positions.is_empty()
    }

    /// Fills that moved a position the fold could not value.
    ///
    /// The position moved and the money did not. Never zero quietly: it
    /// means something is being traded that the caller's valuation does not
    /// model.
    pub const fn unvalued(&self) -> u64 {
        self.unvalued
    }

    /// Move one position to `net` — what the venue says it holds — as
    /// though the difference had traded at `mark`. Reconciliation's one
    /// write into the fold; nothing else may call it.
    ///
    /// The difference is something the fold never saw happen, so it has no
    /// price of its own. Booking it at the book's own mark is the least wrong
    /// of the choices: the open position it leaves carries no unrealised PnL
    /// at that mark, and anything it closes realises against the mark rather
    /// than against a price nobody paid. No fee is charged, because none was
    /// reported; the venue's equity already holds whatever was. Where
    /// there is no mark the net still moves — a quoter that thinks it is
    /// flat keeps selling — and the position's value is dropped and counted
    /// in [`unvalued`](Self::unvalued), exactly as for a fill that could not
    /// be carried.
    ///
    /// A `net` equal to the fold's is nothing to do.
    pub fn rebase(&mut self, instrument: I, net: Qty, mark: Option<Px>) {
        let difference = net - self.net(&instrument);
        if difference == Qty::ZERO {
            return;
        }
        let flat = self.flat(instrument);
        let position = self.positions.entry(instrument).or_insert(flat);
        let valued = match mark {
            Some(price) => position.apply(&Fill {
                // Against no order, like a settlement: the OMS mints ids
                // from one.
                order: ClientOrderId::default(),
                exec_id: ExecId::default(),
                instrument,
                side: if difference > Qty::ZERO {
                    Side::Bid
                } else {
                    Side::Ask
                },
                qty: difference.abs(),
                // One execution for its whole size.
                filled: difference.abs(),
                remaining: Qty::ZERO,
                price,
                fee: Qty::ZERO,
                liquidity: Liquidity::Maker,
                venue_time: None,
                recv_time: NanoTime::ZERO,
            }),
            None => {
                position.net = net;
                position.measure = None;
                position.cost = Qty::ZERO;
                false
            }
        };
        if !valued {
            self.unvalued += 1;
        }
    }

    /// The book's PnL in one settlement currency — the positions `in_ccy`
    /// admits — marking each open position with `mark`.
    ///
    /// One currency at a time, because adding one currency to another is not a number
    /// — positions settling in anything else are not in this total and are
    /// not an error. `None` where a position with a net has no usable mark:
    /// a total that quietly omits an open position is worse than no total.
    /// A flat position needs no mark and does not withhold one.
    pub fn total(
        &self,
        in_ccy: impl Fn(&I) -> bool,
        mark: impl Fn(&I) -> Option<Px>,
    ) -> Option<Pnl> {
        let mut total = Pnl::default();
        for position in self.positions.values() {
            if !in_ccy(&position.instrument) {
                continue;
            }
            if position.is_flat() {
                total = total.plus(Pnl {
                    realised: position.realised,
                    unrealised: Qty::ZERO,
                    fees: position.fees,
                    cashflows: position.cashflows,
                });
                continue;
            }
            total = total.plus(position.pnl(mark(&position.instrument)?)?);
        }
        Some(total)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Settlement currencies, as far as these tests need them.
    #[derive(Clone, Copy, Debug, PartialEq, Eq)]
    enum Ccy {
        Btc,
        Usdc,
    }

    /// A test instrument covering the ways a contract makes money: an
    /// inverse call by strike (sized in coin, premium quoted in coin — so
    /// linear), the inverse perp (sized in dollars, settled in coin), a
    /// linear perp settled in USDC, a BTC/USDC spot pair, and two dollar
    /// futures whose unit is not one price unit — an index future at fifty a
    /// point and a mini at a tenth.
    #[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash, PartialOrd, Ord)]
    enum Inst {
        Call(Px),
        #[default]
        Perp,
        LinearPerp,
        Spot,
        Index,
        Mini,
    }

    type Book = super::Book<Inst>;
    type Position = super::Position<Inst>;
    type Fill = crate::adapters::execution::order::Fill<Inst>;

    /// How this test's caller values its instruments: the coin book and the
    /// two scaled futures. The linear perp and the spot pair are held and not
    /// valued — the caller's choice, and the one the fold must survive.
    fn measure(instrument: &Inst) -> Option<Measure> {
        match instrument {
            Inst::Call(_) => Some(Measure::Linear),
            Inst::Perp => Some(Measure::Inverse),
            Inst::Index => Measure::scaled(qty("50")),
            Inst::Mini => Measure::scaled(qty("0.1")),
            Inst::LinearPerp | Inst::Spot => None,
        }
    }

    fn settles_in(instrument: &Inst) -> Ccy {
        match instrument {
            Inst::Call(_) | Inst::Perp => Ccy::Btc,
            Inst::LinearPerp | Inst::Spot | Inst::Index | Inst::Mini => Ccy::Usdc,
        }
    }

    fn in_ccy(ccy: Ccy) -> impl Fn(&Inst) -> bool {
        move |instrument| settles_in(instrument) == ccy
    }

    fn book() -> Book {
        Book::new(measure)
    }

    fn px(s: &str) -> Px {
        Px::parse(s).unwrap()
    }

    fn qty(s: &str) -> Qty {
        Qty::parse(s).unwrap()
    }

    /// A BTC inverse call — sized in coin, premium quoted in coin.
    fn call() -> Inst {
        strike_at("60000")
    }

    fn strike_at(strike: &str) -> Inst {
        Inst::Call(px(strike))
    }

    /// The inverse perp — sized in dollars, settled in coin.
    fn perp() -> Inst {
        Inst::Perp
    }

    /// The same coin, quoted the other way round: a linear contract, whose
    /// PnL is in the quote currency and which this caller does not value.
    fn linear_perp() -> Inst {
        Inst::LinearPerp
    }

    fn fill(instrument: Inst, side: Side, q: &str, price: &str, fee: &str) -> Fill {
        Fill {
            order: ClientOrderId(1),
            exec_id: ExecId::new("e1").unwrap(),
            instrument,
            side,
            qty: qty(q),
            filled: qty(q),
            remaining: Qty::ZERO,
            price: px(price),
            fee: qty(fee),
            liquidity: Liquidity::Maker,
            venue_time: None,
            recv_time: NanoTime::default(),
        }
    }

    fn free(instrument: Inst, side: Side, q: &str, price: &str) -> Fill {
        fill(instrument, side, q, price, "0")
    }

    // ---------------------------------------------------------------- option

    /// An inverse option is sized in coin and its premium is quoted in coin,
    /// so its PnL is the straight-line one: ten at 0.05 marked at 0.06 is
    /// 0.1 coin.
    #[test]
    fn an_option_position_is_linear_in_the_premium() {
        let mut book = book();
        book.apply(&[free(call(), Side::Bid, "10", "0.05")]);

        let position = book.position(&call()).unwrap();
        assert_eq!(position.net(), qty("10"));
        assert_eq!(position.measure, Some(Measure::Linear));
        assert_eq!(position.entry(), Some(px("0.05")));
        assert_eq!(position.unrealised(px("0.06")), Some(qty("0.1")));
        assert_eq!(position.unrealised(px("0.04")), Some(qty("-0.1")));
        assert_eq!(position.realised(), Qty::ZERO, "nothing is closed");
    }

    /// The v0 trade: sell premium, buy it back cheaper. Realised is booked
    /// on the closing fill and the position goes flat.
    #[test]
    fn a_short_option_bought_back_cheaper_books_the_difference() {
        let mut book = book();
        book.apply(&[
            free(call(), Side::Ask, "10", "0.05"),
            free(call(), Side::Bid, "10", "0.03"),
        ]);

        let position = book.position(&call()).unwrap();
        assert!(position.is_flat());
        assert_eq!(position.realised(), qty("0.2"), "10 × 0.02");
        assert_eq!(position.unrealised(px("0.09")), Some(Qty::ZERO));
        assert_eq!(
            position.entry(),
            None,
            "a flat position was entered at nothing"
        );
    }

    /// A short that has to be bought back dearer loses, which is the whole
    /// risk of the strategy and has to come out with the right sign.
    #[test]
    fn a_short_option_bought_back_dearer_books_a_loss() {
        let mut book = book();
        book.apply(&[
            free(call(), Side::Ask, "10", "0.05"),
            free(call(), Side::Bid, "10", "0.09"),
        ]);
        assert_eq!(book.position(&call()).unwrap().realised(), qty("-0.4"));
    }

    // --------------------------------------------------------------- inverse

    /// The arithmetic this component exists for. Long $60,000 of an inverse
    /// perp at $60,000 is one coin of exposure; at $120,000 it is worth half
    /// a coin, and the PnL is `N × (1/P − 1/M)` — half a coin, not the coin
    /// a straight line in the price would have given.
    #[test]
    fn an_inverse_position_is_linear_in_the_reciprocal() {
        let mut book = book();
        book.apply(&[free(perp(), Side::Bid, "60000", "60000")]);

        let position = book.position(&perp()).unwrap();
        assert_eq!(position.net(), qty("60000"), "dollars, not coins");
        assert_eq!(position.measure, Some(Measure::Inverse));
        assert_eq!(position.entry(), Some(px("60000")));
        assert_eq!(position.cost(), qty("-1"), "the measure is −1/P per unit");
        assert_eq!(position.unrealised(px("120000")), Some(qty("0.5")));

        // A straight line in the price would have said one whole coin, and
        // the error is the same size as the profit.
        assert_ne!(position.unrealised(px("120000")), Some(qty("1")));
    }

    /// The side of the non-linearity that matters to a short gamma book: the
    /// loss on the way down is *larger* than a straight line says, and this
    /// is the test that would fail if the fold booked one.
    #[test]
    fn an_inverse_long_loses_more_than_a_straight_line_on_the_way_down() {
        let mut book = book();
        book.apply(&[free(perp(), Side::Bid, "60000", "60000")]);
        let position = book.position(&perp()).unwrap();

        // Halving the price loses a whole coin: 60000 × (1/60000 − 1/30000).
        assert_eq!(position.unrealised(px("30000")), Some(qty("-1")));
        // Doubling it gains only half of one. The asymmetry is the point.
        assert_eq!(position.unrealised(px("120000")), Some(qty("0.5")));
    }

    /// Closed rather than marked, and both ways round.
    #[test]
    fn an_inverse_position_books_the_same_number_it_marked() {
        let mut long = book();
        long.apply(&[
            free(perp(), Side::Bid, "60000", "60000"),
            free(perp(), Side::Ask, "60000", "120000"),
        ]);
        let position = long.position(&perp()).unwrap();
        assert!(position.is_flat());
        assert_eq!(position.realised(), qty("0.5"));

        let mut short = book();
        short.apply(&[
            free(perp(), Side::Ask, "60000", "60000"),
            free(perp(), Side::Bid, "60000", "30000"),
        ]);
        assert_eq!(
            short.position(&perp()).unwrap().realised(),
            qty("1"),
            "short $60k at 60k, bought back at 30k"
        );
    }

    // ---------------------------------------------------------------- scaled

    /// An index future at fifty dollars a point: long two at 5000.00 marked
    /// at 5001.25 is 1.25 points on two contracts, $125 — not the 2.5 a unit
    /// measure would report in points. The entry reads back in points, and
    /// closing at the mark books the number it marked.
    #[test]
    fn an_index_future_is_worth_its_multiplier_a_point() {
        let mut book = book();
        book.apply(&[free(Inst::Index, Side::Bid, "2", "5000.00")]);

        let position = book.position(&Inst::Index).unwrap();
        assert_eq!(
            position.measure,
            Some(Measure::Scaled {
                multiplier: qty("50")
            })
        );
        assert_eq!(position.net(), qty("2"), "contracts, not dollars");
        assert_eq!(position.cost(), qty("500000"), "2 × 50 × 5000");
        assert_eq!(position.entry(), Some(px("5000.00")));
        assert_eq!(position.unrealised(px("5001.25")), Some(qty("125")));
        assert_eq!(position.unrealised(px("4999")), Some(qty("-100")));

        book.apply(&[free(Inst::Index, Side::Ask, "2", "5001.25")]);
        let position = book.position(&Inst::Index).unwrap();
        assert!(position.is_flat());
        assert_eq!(position.realised(), qty("125"));
        assert_eq!(position.cost(), Qty::ZERO);
        assert_eq!(position.entry(), None);
        assert_eq!(position.unrealised(px("9999")), Some(Qty::ZERO));
    }

    /// The value is two steps so that it has `i128` room to be taken at all:
    /// a million contracts at 5,000 and fifty a point is $250bn, which the
    /// single triple product could not hold.
    #[test]
    fn a_scaled_value_has_room_for_a_large_book() {
        let index = Measure::scaled(qty("50")).unwrap();
        assert_eq!(
            index.value(qty("1000000"), px("5000")),
            Some(qty("250000000000"))
        );
        assert_eq!(
            index.value(qty("-1000000"), px("5000")),
            Some(qty("-250000000000"))
        );
    }

    /// A multiplier below one is as exact as one above it: three minis at a
    /// tenth of a unit a point, entered at an average of 101.37 and marked at
    /// 102, are worth 0.3 × 0.63.
    #[test]
    fn a_fractional_multiplier_is_exact() {
        let mut book = book();
        book.apply(&[
            free(Inst::Mini, Side::Bid, "1", "101.30"),
            free(Inst::Mini, Side::Bid, "2", "101.405"),
        ]);
        let position = book.position(&Inst::Mini).unwrap();
        assert_eq!(
            position.cost(),
            qty("30.411"),
            "0.1 × (101.30 + 2 × 101.405)"
        );
        assert_eq!(position.entry(), Some(px("101.37")));
        assert_eq!(position.unrealised(px("102")), Some(qty("0.189")));
        assert_eq!(position.unrealised(px("101.37")), Some(Qty::ZERO));
    }

    /// The close arithmetic is the measure's, so a multiplier rides through
    /// a partial close and a flip through flat unchanged: each closed
    /// contract books fifty a point, and what flips opens at its own price.
    #[test]
    fn a_scaled_position_closes_in_part_and_flips_through_flat() {
        let mut book = book();
        book.apply(&[
            free(Inst::Index, Side::Bid, "4", "5000"),
            free(Inst::Index, Side::Ask, "1", "5010"),
        ]);
        let position = book.position(&Inst::Index).unwrap();
        assert_eq!(position.net(), qty("3"));
        assert_eq!(position.realised(), qty("500"), "1 × 50 × 10");
        assert_eq!(position.entry(), Some(px("5000")), "the rest is untouched");

        book.apply(&[free(Inst::Index, Side::Ask, "5", "4990")]);
        let position = book.position(&Inst::Index).unwrap();
        assert_eq!(position.net(), qty("-2"));
        assert_eq!(
            position.realised(),
            qty("-1000"),
            "500, then 3 × 50 × −10 on the close"
        );
        assert_eq!(
            position.entry(),
            Some(px("4990")),
            "the new short was opened where it was sold"
        );
        assert_eq!(position.unrealised(px("4990")), Some(Qty::ZERO));
        assert_eq!(position.unrealised(px("4980")), Some(qty("1000")));
    }

    /// A multiplier of zero would value every position at nothing, which
    /// reads as flat PnL rather than a missing number; a negative one would
    /// book a long as a short. Neither is a measure: the constructor refuses
    /// both, the arithmetic answers `None` for a variant built by hand, and a
    /// book handed one folds the net and counts the fill unvalued.
    #[test]
    fn a_multiplier_that_is_not_positive_is_not_a_measure() {
        assert_eq!(Measure::scaled(Qty::ZERO), None);
        assert_eq!(Measure::scaled(qty("-50")), None);
        assert_eq!(
            Measure::scaled(qty("1")),
            Some(Measure::Linear),
            "the unit case has one spelling"
        );

        let zero = Measure::Scaled {
            multiplier: Qty::ZERO,
        };
        let negative = Measure::Scaled {
            multiplier: qty("-50"),
        };
        for measure in [zero, negative] {
            assert_eq!(measure.value(qty("2"), px("5000")), None);
            assert_eq!(measure.price_from(qty("2"), qty("500000")), None);
            assert_eq!(
                Position::flat(Inst::Index, Some(measure)).unrealised(px("5000")),
                None,
                "not even a flat position is valued by it"
            );

            let mut book = Book::new(move |_| Some(measure));
            book.apply(&[free(Inst::Index, Side::Bid, "2", "5000")]);
            let position = book.position(&Inst::Index).unwrap();
            assert_eq!(position.net(), qty("2"), "the net is never withheld");
            assert_eq!(position.measure, None);
            assert_eq!(position.pnl(px("5001")), None);
            assert_eq!(book.unvalued(), 1);
        }
    }

    // ------------------------------------------------------- opening/closing

    /// Adding to a position averages the entry rather than booking anything.
    #[test]
    fn adding_to_a_position_averages_it_and_books_nothing() {
        let mut book = book();
        book.apply(&[
            free(call(), Side::Ask, "10", "0.06"),
            free(call(), Side::Ask, "10", "0.04"),
        ]);
        let position = book.position(&call()).unwrap();
        assert_eq!(position.net(), qty("-20"));
        assert_eq!(position.entry(), Some(px("0.05")));
        assert_eq!(position.realised(), Qty::ZERO);
        assert_eq!(position.unrealised(px("0.05")), Some(Qty::ZERO));
    }

    /// A partial close books its own share and leaves the rest carried where
    /// it was.
    #[test]
    fn a_partial_close_books_its_share_and_leaves_the_entry_alone() {
        let mut book = book();
        book.apply(&[
            free(call(), Side::Ask, "10", "0.05"),
            free(call(), Side::Bid, "4", "0.03"),
        ]);
        let position = book.position(&call()).unwrap();
        assert_eq!(position.net(), qty("-6"));
        assert_eq!(position.realised(), qty("0.08"), "4 × 0.02");
        assert_eq!(position.entry(), Some(px("0.05")), "the rest is untouched");
        assert_eq!(position.unrealised(px("0.05")), Some(Qty::ZERO));
    }

    /// A fill big enough to go through flat books the close and opens the
    /// remainder at its own price — not at the old one.
    #[test]
    fn a_fill_that_flips_the_position_books_the_close_and_opens_the_rest() {
        let mut book = book();
        book.apply(&[
            free(call(), Side::Bid, "10", "0.05"),
            free(call(), Side::Ask, "15", "0.06"),
        ]);
        let position = book.position(&call()).unwrap();
        assert_eq!(position.net(), qty("-5"));
        assert_eq!(position.realised(), qty("0.1"), "10 × 0.01 on the close");
        assert_eq!(
            position.entry(),
            Some(px("0.06")),
            "the new short was opened where it was sold, not where the long was bought"
        );
        assert_eq!(position.unrealised(px("0.06")), Some(Qty::ZERO));
    }

    /// Round-tripping to flat leaves nothing behind that a later mark could
    /// read as a position.
    #[test]
    fn a_closed_position_carries_nothing() {
        let mut book = book();
        book.apply(&[
            free(perp(), Side::Bid, "60000", "60000"),
            free(perp(), Side::Ask, "60000", "61000"),
        ]);
        let position = book.position(&perp()).unwrap();
        assert!(position.is_flat());
        assert_eq!(position.cost(), Qty::ZERO);
        assert_eq!(position.unrealised(px("999999")), Some(Qty::ZERO));
    }

    // ------------------------------------------------------------------ fees

    /// Realised is gross and fees are their own line, because attribution
    /// reports them separately and a number with them already in it could
    /// not be split back out.
    #[test]
    fn fees_are_their_own_line_and_realised_is_gross() {
        let mut book = book();
        book.apply(&[
            fill(call(), Side::Ask, "10", "0.05", "0.001"),
            fill(call(), Side::Bid, "10", "0.03", "0.001"),
        ]);
        let pnl = book.position(&call()).unwrap().pnl(px("0.03")).unwrap();
        assert_eq!(pnl.realised, qty("0.2"), "gross");
        assert_eq!(pnl.fees, qty("0.002"));
        assert_eq!(pnl.unrealised, Qty::ZERO);
        assert_eq!(pnl.net(), qty("0.198"), "what it actually came to");
    }

    /// A maker rebate is a negative fee, which is why the field is signed.
    #[test]
    fn a_rebate_is_a_negative_fee() {
        let mut book = book();
        book.apply(&[fill(call(), Side::Ask, "10", "0.05", "-0.0004")]);
        let position = book.position(&call()).unwrap();
        assert_eq!(position.fees(), qty("-0.0004"));
        assert_eq!(
            position.pnl(px("0.05")).unwrap().net(),
            qty("0.0004"),
            "a rebate is money in"
        );
    }

    /// A fee is charged even on a fill the fold cannot value — the venue
    /// charged it either way.
    #[test]
    fn a_fee_is_charged_on_a_fill_that_cannot_be_valued() {
        let mut book = book();
        book.apply(&[fill(linear_perp(), Side::Bid, "1", "60000", "0.0001")]);
        let position = book.position(&linear_perp()).unwrap();
        assert_eq!(position.fees(), qty("0.0001"));
        assert_eq!(book.unvalued(), 1);
    }

    // ------------------------------------------------- what it will not guess

    /// The split the module docs argue for: the position is always folded,
    /// because a quoter that thinks it is flat keeps selling; the *value* is
    /// refused, because nothing here models a linear contract's PnL.
    #[test]
    fn a_position_it_cannot_value_is_still_a_position() {
        let mut book = book();
        book.apply(&[free(linear_perp(), Side::Bid, "3", "60000")]);

        let position = book.position(&linear_perp()).unwrap();
        assert_eq!(position.net(), qty("3"), "the net is never guessed at");
        assert_eq!(position.measure, None);
        assert_eq!(
            position.pnl(px("61000")),
            None,
            "and the money is not lied about"
        );
        assert_eq!(position.entry(), None);
        assert_eq!(book.unvalued(), 1);
        assert_eq!(book.net(&linear_perp()), qty("3"));
    }

    /// A spot pair has no contract at all, and is delivered rather than
    /// settled. Same treatment, by the same rule.
    #[test]
    fn a_spot_leg_is_held_but_not_valued() {
        let spot = Inst::Spot;
        let mut book = book();
        book.apply(&[free(spot, Side::Bid, "1", "60000")]);
        assert_eq!(book.net(&spot), qty("1"));
        assert_eq!(book.position(&spot).unwrap().pnl(px("61000")), None);
        assert_eq!(book.unvalued(), 1);
    }

    // ------------------------------------------------------------- the book

    /// What the quoter reads. A contract nobody has traded is flat, and
    /// "flat" and "absent" are the same fact about it.
    #[test]
    fn an_untraded_instrument_is_flat_rather_than_missing() {
        let book = book();
        assert_eq!(book.net(&call()), Qty::ZERO);
        assert_eq!(book.position(&call()), None);
        assert!(book.is_empty());
    }

    /// Adding coin to dollars is not a number, so a total says which
    /// currency it is in and leaves the rest out.
    #[test]
    fn a_total_is_one_currency_at_a_time() {
        let mut book = book();
        book.apply(&[
            fill(call(), Side::Ask, "10", "0.05", "0.001"),
            fill(perp(), Side::Bid, "60000", "60000", "0.0002"),
            // A linear contract settles in the quote currency, so it is not
            // in a coin total at all.
            fill(linear_perp(), Side::Bid, "1", "60000", "0.5"),
        ]);

        let marks = |instrument: &Inst| match *instrument {
            i if i == call() => Some(px("0.03")),
            i if i == perp() => Some(px("120000")),
            _ => Some(px("60000")),
        };
        let total = book
            .total(in_ccy(Ccy::Btc), marks)
            .expect("every coin leg is marked");
        assert_eq!(
            total.unrealised,
            qty("0.7"),
            "0.2 on the option, 0.5 on the perp"
        );
        assert_eq!(
            total.fees,
            qty("0.0012"),
            "the linear leg's fee is not coin"
        );
        assert_eq!(total.realised, Qty::ZERO);
        assert_eq!(total.net(), qty("0.6988"));
    }

    /// A total that quietly omits an open position is worse than no total.
    #[test]
    fn a_total_refuses_rather_than_omit_an_open_position() {
        let mut book = book();
        book.apply(&[free(call(), Side::Ask, "10", "0.05")]);
        assert_eq!(book.total(in_ccy(Ccy::Btc), |_| None), None);

        // Closed out, it needs no mark and does not withhold one.
        book.apply(&[free(call(), Side::Bid, "10", "0.03")]);
        let total = book
            .total(in_ccy(Ccy::Btc), |_| None)
            .expect("a flat book needs no mark");
        assert_eq!(total.realised, qty("0.2"));
    }

    /// Funding is a cashflow: it moves what the position has earned and
    /// leaves what it holds, its cost and its entry alone — and it is its
    /// own field, so 20 can attribute it rather than leave it in the
    /// residual.
    #[test]
    fn funding_is_booked_beside_the_position_and_moves_nothing_it_holds() {
        let mut book = book();
        book.apply(&[fill(perp(), Side::Bid, "60000", "60000", "0.0002")]);
        let before = book.position(&perp()).unwrap();

        // A long pays while the perp trades over the index, and is paid
        // back when it trades under.
        book.cashflow(perp(), qty("-0.00003"));
        book.cashflow(perp(), qty("0.00001"));

        let after = book.position(&perp()).unwrap();
        assert_eq!(after.net(), before.net());
        assert_eq!(after.cost(), before.cost());
        assert_eq!(after.entry(), before.entry());
        assert_eq!(after.cashflows(), qty("-0.00002"));
        let pnl = after.pnl(px("60000")).unwrap();
        assert_eq!(pnl.cashflows, qty("-0.00002"));
        assert_eq!(
            pnl.net(),
            qty("-0.00022"),
            "funding and the fee, nothing else"
        );
        let total = book.total(in_ccy(Ccy::Btc), |_| Some(px("60000"))).unwrap();
        assert_eq!(total.net(), qty("-0.00022"));

        // Closed out, a flat position still holds what it was charged.
        book.apply(&[free(perp(), Side::Ask, "60000", "60000")]);
        let flat = book.position(&perp()).unwrap();
        assert_eq!(flat.booked(), qty("-0.00022"));
        assert_eq!(
            book.total(in_ccy(Ccy::Btc), |_| None).unwrap().net(),
            qty("-0.00022")
        );
    }

    /// A payment on a position this fold never saw — a restart onto a book
    /// the account already held — is still money that moved.
    #[test]
    fn funding_on_an_unheld_instrument_is_still_booked() {
        let mut book = book();
        book.cashflow(perp(), qty("0.0001"));
        assert_eq!(book.net(&perp()), Qty::ZERO);
        assert_eq!(book.position(&perp()).unwrap().cashflows(), qty("0.0001"));
        assert_eq!(
            book.total(in_ccy(Ccy::Btc), |_| None).unwrap().cashflows,
            qty("0.0001")
        );
    }

    /// The quote-currency view. The position is already denominated in coin,
    /// so the index is the whole conversion.
    #[test]
    fn a_coin_pnl_converts_at_the_index() {
        let mut book = book();
        book.apply(&[
            free(call(), Side::Ask, "10", "0.05"),
            free(call(), Side::Bid, "10", "0.03"),
        ]);
        let coin = book.total(in_ccy(Ccy::Btc), |_| Some(px("0.03"))).unwrap();
        let usd = coin.in_quote(px("60000")).unwrap();
        assert_eq!(usd.realised, qty("12000"), "0.2 coin at $60,000");
        assert_eq!(usd.net(), qty("12000"));
    }

    /// Anything that builds a sequence out of the book gets the instrument's
    /// order, whatever order the fills arrived in — the same parity the OMS
    /// and the sim protect.
    #[test]
    fn the_positions_come_out_in_a_stated_order() {
        let strikes = ["70000", "60000", "65000"];
        let listed = |order: &[usize]| -> Vec<Inst> {
            let mut book = book();
            for i in order {
                book.apply(&[free(strike_at(strikes[*i]), Side::Bid, "1", "0.05")]);
            }
            book.positions()
                .into_iter()
                .map(|position| position.instrument)
                .collect()
        };
        let first = listed(&[0, 1, 2]);
        assert_eq!(first.len(), 3);
        assert_eq!(listed(&[2, 0, 1]), first);
        assert_eq!(listed(&[1, 2, 0]), first);
        let mut sorted = first.clone();
        sorted.sort_unstable();
        assert_eq!(first, sorted);

        // `iter` walks the same map, so it is ordered too.
        let mut book = book();
        for strike in strikes {
            book.apply(&[free(strike_at(strike), Side::Bid, "1", "0.05")]);
        }
        let walked: Vec<Inst> = book.iter().map(|position| position.instrument).collect();
        assert_eq!(walked, first);
    }

    /// The valuation is a closure, so a caller whose measures live in a
    /// registry — here a map of instrument to measure — captures it rather
    /// than writing a function per contract. What the map does not name is
    /// unvalued, as a `None` from a function is, and a clone of the book
    /// values as the original does.
    #[test]
    fn the_valuation_can_be_a_closure_over_a_registry() {
        let registry: std::collections::HashMap<Inst, Measure> =
            [(call(), Measure::Linear), (perp(), Measure::Inverse)].into();
        let mut book = Book::new(move |instrument: &Inst| registry.get(instrument).copied());
        book.apply(&[
            free(call(), Side::Bid, "10", "0.05"),
            free(linear_perp(), Side::Bid, "1", "60000"),
        ]);
        assert_eq!(
            book.position(&call()).unwrap().measure,
            Some(Measure::Linear)
        );
        assert_eq!(book.position(&linear_perp()).unwrap().measure, None);
        assert_eq!(book.unvalued(), 1);

        let mut clone = book.clone();
        clone.apply(&[free(perp(), Side::Ask, "60000", "60000")]);
        assert_eq!(
            clone.position(&perp()).unwrap().measure,
            Some(Measure::Inverse)
        );
        assert!(format!("{clone:?}").contains("unvalued: 1"), "{clone:?}");
    }

    /// A burst is applied in order, and the order is what decides which
    /// fills open and which close.
    #[test]
    fn a_burst_is_applied_in_order() {
        let mut in_order = book();
        in_order.apply(&[
            free(call(), Side::Ask, "10", "0.05"),
            free(call(), Side::Bid, "10", "0.03"),
            free(call(), Side::Ask, "10", "0.07"),
        ]);
        let position = in_order.position(&call()).unwrap();
        assert_eq!(position.net(), qty("-10"));
        assert_eq!(position.realised(), qty("0.2"));
        assert_eq!(position.entry(), Some(px("0.07")));
    }

    /// Reconciliation's write (18b): the fold moves to the venue's net, the
    /// difference booked at the mark — so a position it opens carries no
    /// unrealised PnL there, and one it closes realises against the mark.
    #[test]
    fn a_rebase_moves_the_net_at_the_mark() {
        let mut book = book();
        // A short the fold never saw, found at a mark of 0.04.
        book.rebase(call(), qty("-2"), Some(px("0.04")));
        let position = book.position(&call()).unwrap();
        assert_eq!(position.net(), qty("-2"));
        assert_eq!(position.entry(), Some(px("0.04")));
        assert_eq!(position.unrealised(px("0.04")), Some(Qty::ZERO));
        assert_eq!(position.fees(), Qty::ZERO, "nothing was reported charged");

        // The venue then says half of it is gone, at a mark of 0.03: the
        // closed half realises against the mark it was carried at.
        book.rebase(call(), qty("-1"), Some(px("0.03")));
        let position = book.position(&call()).unwrap();
        assert_eq!(position.net(), qty("-1"));
        assert_eq!(position.realised(), qty("0.01"));
        assert_eq!(book.unvalued(), 0);

        // Agreeing is nothing to do.
        book.rebase(call(), qty("-1"), None);
        assert_eq!(book.position(&call()).unwrap(), position);
    }

    /// With no mark the net still moves — it is the number that must never
    /// be wrong — and the value is dropped and counted.
    #[test]
    fn a_rebase_without_a_mark_moves_the_net_and_drops_the_value() {
        let mut book = book();
        book.apply(&[free(perp(), Side::Bid, "1000", "60000")]);
        book.rebase(perp(), qty("3000"), None);
        let position = book.position(&perp()).unwrap();
        assert_eq!(position.net(), qty("3000"));
        assert_eq!(position.measure, None);
        assert_eq!(position.pnl(px("60000")), None);
        assert_eq!(book.unvalued(), 1);
    }

    /// A book restored from what [`Book::positions`] listed is
    /// the book that listed it — the entry price, the realised and every
    /// line of PnL carry across, and a fill after the restart closes against
    /// the cost the old process paid rather than a mark.
    #[test]
    fn a_restored_book_is_the_book_it_was() {
        let mut before = book();
        before.apply(&[free(call(), Side::Ask, "2", "0.05")]);
        before.apply(&[free(call(), Side::Bid, "1", "0.03")]);
        before.apply(&[free(perp(), Side::Bid, "1000", "60000")]);
        before.cashflow(perp(), qty("0.0001"));
        before.rebase(perp(), qty("3000"), None);

        let listed = before.positions();
        let after = Book::restore(
            measure,
            listed.iter().map(|p| {
                Position::restore(
                    p.instrument,
                    p.measure,
                    p.net(),
                    p.cost(),
                    p.realised(),
                    p.fees(),
                    p.cashflows(),
                )
            }),
            before.unvalued(),
        );
        assert_eq!(after.positions(), listed);
        assert_eq!(after.unvalued(), before.unvalued());
        let call_before = before.position(&call()).unwrap();
        let call_after = after.position(&call()).unwrap();
        assert_eq!(call_after.entry(), call_before.entry());
        assert_eq!(call_after.entry(), Some(px("0.05")));
        assert_eq!(call_after.realised(), qty("0.02"));

        let (mut before, mut after) = (before, after);
        let close = free(call(), Side::Bid, "1", "0.01");
        before.apply(&[close]);
        after.apply(&[close]);
        assert_eq!(after.positions(), before.positions());
        assert_eq!(after.position(&call()).unwrap().realised(), qty("0.06"));
    }
}
