//! The position and PnL fold over a stream of [`Fill`]s.
//!
//! Deliberately the dullest part of the layer: position keeping is a fold,
//! risk is a filter, PnL is a join. What makes it worth writing once is that
//! average-price accounting has a handful of edge cases — closing through
//! zero, flipping sign, fees — that every consumer would otherwise get subtly
//! wrong in its own way.

use anyhow::{Result, bail};

use super::{Fill, Notional, notional_of, scaled_mul};
use crate::adapters::market::{InstrumentId, Px, Qty};
use crate::fluent::Stream;
use crate::op::{Activation, Ctx, Op, Tick};
use crate::{Burst, NanoTime, op};

/// A net position in one instrument, with realized and unrealized PnL.
///
/// Average-price accounting: opening or increasing re-weights
/// [`avg_price`](Position::avg_price); reducing or closing realizes PnL
/// against it; crossing through zero realizes the whole old position and
/// re-opens the remainder at the fill price.
///
/// The `Default` impl (an empty instrument, flat) exists only because the
/// engine requires every stream's value type to be `Default`.
#[derive(Clone, Debug, Default, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Position {
    instrument: InstrumentId,
    net_qty: Qty,
    avg_price: Option<Px>,
    realized_pnl: Notional,
    fees: Notional,
    fill_count: usize,
    last_fill_time: NanoTime,
}

impl Position {
    /// A flat position in `instrument`.
    pub fn new(instrument: InstrumentId) -> Self {
        Self {
            instrument,
            ..Default::default()
        }
    }

    /// The instrument this position is in.
    pub fn instrument(&self) -> &InstrumentId {
        &self.instrument
    }

    /// The net quantity held: positive long, negative short, zero flat.
    pub const fn net_qty(&self) -> Qty {
        self.net_qty
    }

    /// The average price of the open position, or `None` while flat.
    pub const fn avg_price(&self) -> Option<Px> {
        self.avg_price
    }

    /// PnL locked in by closing, **before** fees.
    pub const fn realized_pnl(&self) -> Notional {
        self.realized_pnl
    }

    /// Fees paid so far, positive for a cost.
    pub const fn fees(&self) -> Notional {
        self.fees
    }

    /// How many fills have been folded in.
    pub const fn fill_count(&self) -> usize {
        self.fill_count
    }

    /// Engine time of the most recent fill.
    pub const fn last_fill_time(&self) -> NanoTime {
        self.last_fill_time
    }

    /// Whether the position is flat.
    pub const fn is_flat(&self) -> bool {
        self.net_qty.is_zero()
    }

    /// PnL on the open position if it were marked at `mark`, before fees.
    ///
    /// Zero while flat.
    ///
    /// # Errors
    ///
    /// If the mark-to-market product overflows the fixed-point range.
    pub fn unrealized_pnl(&self, mark: Px) -> Result<Notional> {
        let Some(avg) = self.avg_price else {
            return Ok(Notional::ZERO);
        };
        // Signed on both factors: a short (negative qty) marked below its
        // average is a gain, and the two negatives handle that without a
        // branch on side.
        scaled_mul(mark.raw() - avg.raw(), self.net_qty.raw()).map(Notional::from_raw)
    }

    /// Realized plus unrealized PnL, **net of fees** — the number a strategy
    /// is actually judged on.
    ///
    /// # Errors
    ///
    /// As [`unrealized_pnl`](Position::unrealized_pnl).
    pub fn total_pnl(&self, mark: Px) -> Result<Notional> {
        let unrealized = self.unrealized_pnl(mark)?;
        Ok(Notional::from_raw(
            self.realized_pnl.raw() + unrealized.raw() - self.fees.raw(),
        ))
    }

    /// Fold one fill into the position.
    ///
    /// # Errors
    ///
    /// If the fill is for a different instrument, or if the arithmetic
    /// overflows the fixed-point range.
    pub fn apply(&mut self, fill: &Fill) -> Result<()> {
        if self.instrument.venue.as_str().is_empty() && self.instrument.symbol.as_str().is_empty() {
            self.instrument = fill.instrument.clone();
        } else if !self.instrument.same_as(&fill.instrument) {
            bail!(
                "position in {} received a fill for {}; demultiplex by instrument \
                 before folding a position",
                self.instrument,
                fill.instrument
            );
        }

        let delta = fill.signed_qty();
        let old_qty = self.net_qty;
        let new_qty = Qty::from_raw(old_qty.raw().checked_add(delta.raw()).ok_or_else(|| {
            anyhow::anyhow!("position quantity overflows on fill {}", fill.exec_id)
        })?);

        let opening = old_qty.is_zero() || (old_qty.raw() > 0) == (delta.raw() > 0);
        if opening {
            // Re-weight the average over the combined size. Both terms are
            // price×quantity at 2×SCALE, so the divide by the new quantity
            // lands back at price scale with no extra rescaling.
            let old_cost = self.avg_price.map_or(Ok(0), |avg| {
                avg.raw().checked_mul(old_qty.raw()).ok_or_else(|| {
                    anyhow::anyhow!("position cost overflows on fill {}", fill.exec_id)
                })
            })?;
            let add_cost =
                fill.price.raw().checked_mul(delta.raw()).ok_or_else(|| {
                    anyhow::anyhow!("fill cost overflows on fill {}", fill.exec_id)
                })?;
            self.avg_price = Some(Px::from_raw((old_cost + add_cost) / new_qty.raw()));
        } else {
            let avg = self.avg_price.ok_or_else(|| {
                anyhow::anyhow!("invariant: a non-flat position has an average price")
            })?;
            // The quantity actually closed is capped by what was open — the
            // rest, if any, opens a new position on the other side.
            let closed = old_qty.raw().abs().min(delta.raw().abs());
            let direction = if old_qty.raw() > 0 { 1 } else { -1 };
            let realized = scaled_mul((fill.price.raw() - avg.raw()) * direction, closed)?;
            self.realized_pnl = Notional::from_raw(self.realized_pnl.raw() + realized);

            self.avg_price = if new_qty.is_zero() {
                None
            } else if (new_qty.raw() > 0) != (old_qty.raw() > 0) {
                // Flipped through zero: the remainder opens here.
                Some(fill.price)
            } else {
                Some(avg)
            };
        }

        self.net_qty = new_qty;
        self.fees = Notional::from_raw(self.fees.raw() + fill.fee.raw());
        self.fill_count += 1;
        self.last_fill_time = fill.recv_time;
        Ok(())
    }

    /// The position's notional exposure at `mark`, signed by direction.
    ///
    /// # Errors
    ///
    /// If the product overflows the fixed-point range.
    pub fn exposure(&self, mark: Px) -> Result<Notional> {
        notional_of(mark, self.net_qty)
    }
}

// -------------------------------------------------------------------------
// The ops.
// -------------------------------------------------------------------------

/// Folds a [`Position`] from a stream of [`Fill`]s.
pub struct PositionOp;

#[op(build = position)]
impl Op for PositionOp {
    type Cfg = ();
    type State = Position;
    type In<'a> = (&'a Fill,);
    type Out = Position;
    const ACTIVATION: Activation = Activation::NONE;

    fn cycle(
        _cfg: &mut (),
        state: &mut Position,
        input: (&Fill,),
        _ctx: &mut Ctx<'_>,
    ) -> Result<Tick<Position>> {
        state.apply(input.0)?;
        Ok(Tick::Value(state.clone()))
    }
}

/// Folds a [`Position`] from same-instant [`Burst`]s of fills.
///
/// The burst form is the one the execution edge actually carries — one order
/// crossing several levels fills several times at the same instant — and it is
/// where "never latest-wins" earns its keep: a position is a fold over its
/// whole fill history, so collapsing a burst to its last fill silently
/// mis-states the size.
///
/// One tick per burst, carrying the position after the whole group is applied.
pub struct PositionBurstOp;

#[op(build = position_bursts)]
impl Op for PositionBurstOp {
    type Cfg = ();
    type State = Position;
    type In<'a> = (&'a Burst<Fill>,);
    type Out = Position;
    const ACTIVATION: Activation = Activation::NONE;

    fn cycle(
        _cfg: &mut (),
        state: &mut Position,
        input: (&Burst<Fill>,),
        _ctx: &mut Ctx<'_>,
    ) -> Result<Tick<Position>> {
        if input.0.is_empty() {
            return Ok(Tick::Quiet);
        }
        for fill in input.0.iter() {
            state.apply(fill)?;
        }
        Ok(Tick::Value(state.clone()))
    }
}

/// The position fold on a stream of fills.
///
/// Not in the [`prelude`](crate::prelude) — `use
/// wingfoil::adapters::execution::PositionOps;`.
pub trait PositionOps {
    /// Fold this stream of fills into a running [`Position`].
    ///
    /// Ticks once per fill (or once per burst of them), carrying the position
    /// after they are applied. The stream must carry one instrument; a second
    /// one is an error that aborts the run.
    fn position(&self) -> Stream<Position>;
}

impl PositionOps for Stream<Fill> {
    fn position(&self) -> Stream<Position> {
        self.wire(|b, h| b.position(h))
    }
}

impl PositionOps for Stream<Burst<Fill>> {
    fn position(&self) -> Stream<Position> {
        self.wire(|b, h| b.position_bursts(h))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::adapters::execution::{ClOrdId, ExecId};
    use crate::adapters::market::Side;

    fn inst() -> InstrumentId {
        InstrumentId::new("test", "BTC-USD")
    }

    fn fill(side: Side, qty: &str, price: &str) -> Fill {
        Fill {
            exec_id: ExecId::new("e"),
            cl_ord_id: ClOrdId::new("o"),
            instrument: inst(),
            side,
            qty: Qty::parse(qty).unwrap(),
            price: Px::parse(price).unwrap(),
            cum_qty: Qty::parse(qty).unwrap(),
            leaves_qty: Qty::ZERO,
            fee: Notional::ZERO,
            venue_time: None,
            recv_time: NanoTime::ZERO,
        }
    }

    #[test]
    fn buying_twice_averages_the_entry() {
        let mut p = Position::new(inst());
        p.apply(&fill(Side::Bid, "1", "100")).unwrap();
        p.apply(&fill(Side::Bid, "3", "104")).unwrap();
        assert_eq!(p.net_qty(), Qty::parse("4").unwrap());
        assert_eq!(p.avg_price(), Some(Px::parse("103").unwrap()));
        assert_eq!(p.realized_pnl(), Notional::ZERO);
    }

    #[test]
    fn selling_part_realizes_against_the_average() {
        let mut p = Position::new(inst());
        p.apply(&fill(Side::Bid, "4", "100")).unwrap();
        p.apply(&fill(Side::Ask, "1", "110")).unwrap();
        assert_eq!(p.net_qty(), Qty::parse("3").unwrap());
        // The average survives a partial close.
        assert_eq!(p.avg_price(), Some(Px::parse("100").unwrap()));
        assert_eq!(p.realized_pnl(), Notional::parse("10").unwrap());
        assert_eq!(
            p.unrealized_pnl(Px::parse("110").unwrap()).unwrap(),
            Notional::parse("30").unwrap()
        );
    }

    #[test]
    fn closing_flat_drops_the_average() {
        let mut p = Position::new(inst());
        p.apply(&fill(Side::Bid, "2", "100")).unwrap();
        p.apply(&fill(Side::Ask, "2", "90")).unwrap();
        assert!(p.is_flat());
        assert_eq!(p.avg_price(), None);
        assert_eq!(p.realized_pnl(), Notional::parse("-20").unwrap());
        assert_eq!(
            p.unrealized_pnl(Px::parse("1000").unwrap()).unwrap(),
            Notional::ZERO
        );
    }

    #[test]
    fn flipping_through_zero_realizes_the_old_side_and_reopens() {
        let mut p = Position::new(inst());
        p.apply(&fill(Side::Bid, "2", "100")).unwrap();
        p.apply(&fill(Side::Ask, "5", "110")).unwrap();
        // The long two realized +20; the remaining three are short from 110.
        assert_eq!(p.net_qty(), Qty::parse("-3").unwrap());
        assert_eq!(p.avg_price(), Some(Px::parse("110").unwrap()));
        assert_eq!(p.realized_pnl(), Notional::parse("20").unwrap());
    }

    #[test]
    fn a_short_gains_when_the_mark_falls() {
        let mut p = Position::new(inst());
        p.apply(&fill(Side::Ask, "2", "100")).unwrap();
        assert_eq!(p.net_qty(), Qty::parse("-2").unwrap());
        assert_eq!(
            p.unrealized_pnl(Px::parse("90").unwrap()).unwrap(),
            Notional::parse("20").unwrap()
        );
        assert_eq!(
            p.unrealized_pnl(Px::parse("105").unwrap()).unwrap(),
            Notional::parse("-10").unwrap()
        );
    }

    #[test]
    fn fees_come_off_the_total_but_not_the_realized() {
        let mut p = Position::new(inst());
        let mut buy = fill(Side::Bid, "1", "100");
        buy.fee = Notional::parse("0.5").unwrap();
        p.apply(&buy).unwrap();
        let mut sell = fill(Side::Ask, "1", "110");
        sell.fee = Notional::parse("0.55").unwrap();
        p.apply(&sell).unwrap();

        assert_eq!(p.realized_pnl(), Notional::parse("10").unwrap());
        assert_eq!(p.fees(), Notional::parse("1.05").unwrap());
        assert_eq!(
            p.total_pnl(Px::parse("110").unwrap()).unwrap(),
            Notional::parse("8.95").unwrap()
        );
    }

    #[test]
    fn a_fill_for_another_instrument_is_an_error() {
        let mut p = Position::new(inst());
        let mut other = fill(Side::Bid, "1", "100");
        other.instrument = InstrumentId::new("test", "ETH-USD");
        let error = p.apply(&other).unwrap_err();
        assert!(error.to_string().contains("demultiplex"), "{error}");
    }

    #[test]
    fn a_default_position_adopts_the_first_fills_instrument() {
        let mut p = Position::default();
        p.apply(&fill(Side::Bid, "1", "100")).unwrap();
        assert_eq!(p.instrument(), &inst());
    }
}
