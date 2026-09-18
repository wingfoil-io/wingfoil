//! A simulated venue: [`SimVenueOps::sim_venue`] matches a stream of
//! [`Order`]s against a replayed [`OrderBook`] and emits the [`Fill`]s.
//!
//! # Phase 0 is deliberately dishonest, and that is the point
//!
//! This model **fills at touch, with no queue position, no fees, no market
//! impact and no modelled latency**. It is known to lie, and it lies in the
//! flattering direction: a passive order here fills the moment the touch
//! reaches its price, where a real one would be behind an unknown queue and
//! might never fill at all.
//!
//! It ships that way on purpose. The deliverable of gate P0
//! (`docs/planning/proposals/trading-stack.md` §4.1) is **the closed loop** —
//! strategy → order → fill → position → strategy — not fill quality. Whether
//! the loop closes depends on cycle semantics, not on how good the fill model
//! is, and fill quality is an unbounded axis of modelling opinions that must
//! not sit on the critical path to finding that out. An honest model — queue
//! position, fees, configurable latency — is gate P2, and it replaces the
//! matcher in this file without touching anything above it.
//!
//! **So do not measure a strategy with this.** Its numbers are an upper bound
//! and nothing else.
//!
//! # The two decisions this simulator makes explicitly
//!
//! By analogy with [`market`](crate::adapters::market)'s "five decisions an
//! adapter must not make for itself", these are the ones a sim venue must
//! *state* rather than let fall out of wiring order. Each has real PnL
//! consequences and each is invisible in a backtest that gets it wrong.
//!
//! **1. An order matches the book as of the previous instant.** When an order
//! and a book update arrive in the same cycle, the order is matched *before*
//! the update is adopted. Matching after would let a strategy react to a book
//! update and fill against that same update at the same instant — lookahead,
//! dressed up as speed. Resting orders are re-matched on each book tick, also
//! against the previous instant's book, so nothing fills against an update it
//! could not have seen.
//!
//! The engine does **not** settle this for you.
//! [`feedback`](crate::fluent::StreamOps::feedback)'s `time + 1` guarantees
//! the *strategy* cannot observe a fill at the instant it ordered; the
//! question of which side of the sim's own cycle the book update lands on
//! recurs one cycle later, inside here, and the engine takes no position on
//! it. `tests/execution_loop.rs` pins the answer.
//!
//! **2. Latency is modelled in engine time, never the wall clock.** P0 adds no
//! delay of its own beyond the structural one-nanosecond floor that
//! [`feedback`](crate::fluent::StreamOps::feedback) imposes on the order hop.
//! A realistic ack/fill delay is a larger delay scheduled on the same
//! mechanism — [`Ctx::time`](crate::op::Ctx::time), never
//! [`Ctx::wall_time`](crate::op::Ctx::wall_time), or the determinism the whole
//! layer rests on evaporates at exactly the boundary being simulated.
//!
//! # What the matcher does
//!
//! Against the previous instant's book, walking the opposite side from the
//! touch outwards:
//!
//! - A [`OrderType::Market`] order takes levels until it is filled or the side
//!   is exhausted.
//! - A [`OrderType::Limit`] order takes levels whose price is at or better
//!   than its limit.
//! - [`TimeInForce::FillOrKill`] fills in full or not at all;
//!   [`TimeInForce::ImmediateOrCancel`] takes what it can and cancels the
//!   rest; [`TimeInForce::Day`] and [`TimeInForce::GoodTillCancel`] rest the
//!   remainder (a market order never rests — its remainder is always
//!   cancelled).
//! - Crossing several levels produces several [`Fill`]s **in one burst**, each
//!   at its own level's price, with [`Fill::cum_qty`] and
//!   [`Fill::leaves_qty`] running through the group.
//!
//! Taking liquidity does **not** remove it from the book: the book is replayed
//! data, so a fill here has no market impact. That is another P0 lie, and
//! another thing P2 owns.
//!
//! # Layering
//!
//! Not in the [`prelude`](crate::prelude) — `use
//! wingfoil::adapters::execution::sim::SimVenueOps;`.

use anyhow::Result;
use std::sync::Arc;

use super::{ClOrdId, ExecId, Fill, Notional, Order, OrderType, TimeInForce};
use crate::adapters::market::{OrderBook, Px, Qty, Side};
use crate::fluent::Stream;
use crate::op::{Activation, Ctx, Op, Tick};
use crate::{Burst, op};

/// An order the simulator is still working, with what is left of it.
#[derive(Clone, Debug, Default, PartialEq)]
struct Working {
    order: Order,
    cum_qty: Qty,
    leaves_qty: Qty,
}

/// The simulator's engine-owned state: the book as of the previous instant,
/// the orders still working, and the execution-id counter.
#[derive(Debug, Default)]
pub struct SimVenueState {
    /// The book as it stood at the end of the last cycle in which it ticked —
    /// *not* this cycle's book. Decision 1 in the module docs is this field.
    book: Option<Arc<OrderBook>>,
    working: Vec<Working>,
    next_exec_id: u64,
}

impl SimVenueState {
    /// How many orders are still resting. Exposed for tests and diagnostics.
    pub fn working_orders(&self) -> usize {
        self.working.len()
    }
}

/// Matches orders against a replayed book — see the module docs for the model
/// and its deliberate limits.
pub struct SimVenue;

#[op(build = sim_venue)]
impl Op for SimVenue {
    type Cfg = ();
    type State = SimVenueState;
    /// Book value, book tick, order value, order tick — the two-edge
    /// `(value, tick)`-per-edge form. Both flags are load-bearing: without
    /// them a cycle driven by a book update would re-match the *previous*
    /// cycle's orders, filling every order twice.
    type In<'a> = (&'a Arc<OrderBook>, bool, &'a Burst<Order>, bool);
    type Out = Burst<Fill>;
    const ACTIVATION: Activation = Activation::NONE;

    fn cycle(
        _cfg: &mut (),
        state: &mut SimVenueState,
        input: (&Arc<OrderBook>, bool, &Burst<Order>, bool),
        ctx: &mut Ctx<'_>,
    ) -> Result<Tick<Burst<Fill>>> {
        let (book, book_ticked, orders, orders_ticked) = input;
        let now = ctx.time();
        let mut fills: Burst<Fill> = Burst::new();

        // Everything below matches against `state.book` — the book as of the
        // previous instant — and only then is this cycle's book adopted.
        if book_ticked {
            // A new book means the resting orders get one look at the book
            // they could legitimately have seen. On an orders-only cycle they
            // have already had it, and re-matching the same book cannot fill
            // anything a fill has no impact on.
            let previous = state.book.clone();
            if let Some(previous) = previous.as_deref() {
                fill_working(state, previous, now, &mut fills)?;
            }
        }

        if orders_ticked {
            for order in orders.iter() {
                order.validate()?;
                let mut working = Working {
                    cum_qty: Qty::ZERO,
                    leaves_qty: order.qty,
                    order: order.clone(),
                };
                if let Some(previous) = state.book.clone().as_deref() {
                    match_one(state, &mut working, previous, now, &mut fills)?;
                }
                if !working.leaves_qty.is_zero() && rests(&working.order) {
                    state.working.push(working);
                }
            }
        }

        if book_ticked {
            state.book = Some(Arc::clone(book));
        }

        Ok(if fills.is_empty() {
            Tick::Quiet
        } else {
            Tick::Value(fills)
        })
    }
}

/// Whether an order's remainder rests rather than being cancelled.
///
/// A market order never rests whatever its time in force says — resting
/// without a price is not a thing a venue can do.
fn rests(order: &Order) -> bool {
    order.order_type == OrderType::Limit && order.time_in_force.rests()
}

/// Give every working order one pass against `book`, dropping the ones that
/// finish.
fn fill_working(
    state: &mut SimVenueState,
    book: &OrderBook,
    now: crate::NanoTime,
    fills: &mut Burst<Fill>,
) -> Result<()> {
    // Taken out and put back so `match_one` can hold `&mut state` for the
    // execution-id counter; the orders are re-pushed in their original order,
    // which is the time priority they were given on arrival.
    let mut working = std::mem::take(&mut state.working);
    for order in working.iter_mut() {
        match_one(state, order, book, now, fills)?;
    }
    working.retain(|w| !w.leaves_qty.is_zero());
    debug_assert!(
        state.working.is_empty(),
        "invariant: match_one never rests an already-working order"
    );
    state.working = working;
    Ok(())
}

/// Match one working order against `book`, appending its fills.
fn match_one(
    state: &mut SimVenueState,
    working: &mut Working,
    book: &OrderBook,
    now: crate::NanoTime,
    fills: &mut Burst<Fill>,
) -> Result<()> {
    let order = &working.order;
    let taking = order.side.opposite();
    let levels = book.depth(taking, book.level_count(taking));

    // Walk the opposite side from the touch outwards, stopping at the first
    // level the order's limit cannot reach.
    let mut takeable: Vec<(Px, Qty)> = Vec::new();
    let mut remaining = working.leaves_qty.raw();
    for level in levels {
        if remaining <= 0 {
            break;
        }
        if !crosses(order, level.price) {
            break;
        }
        let take = remaining.min(level.qty.raw());
        if take <= 0 {
            continue;
        }
        takeable.push((level.price, Qty::from_raw(take)));
        remaining -= take;
    }

    // Fill-or-kill is all or nothing, so it is decided before anything is
    // emitted rather than unwound afterwards.
    if order.time_in_force == TimeInForce::FillOrKill && remaining > 0 {
        working.leaves_qty = Qty::ZERO;
        return Ok(());
    }

    for (price, qty) in takeable {
        state.next_exec_id += 1;
        working.cum_qty = Qty::from_raw(working.cum_qty.raw() + qty.raw());
        working.leaves_qty = Qty::from_raw(working.leaves_qty.raw() - qty.raw());
        fills.push(Fill {
            exec_id: ExecId::new(format!("sim-{}", state.next_exec_id)),
            cl_ord_id: ClOrdId(order.cl_ord_id.0.clone()),
            instrument: order.instrument.clone(),
            side: order.side,
            qty,
            price,
            cum_qty: working.cum_qty,
            leaves_qty: working.leaves_qty,
            // P0 charges nothing, and says so loudly in the module docs.
            fee: Notional::ZERO,
            // A simulated venue has no clock of its own that is worth more
            // than engine time, and engine time is what replays.
            venue_time: Some(now),
            recv_time: now,
        });
    }

    // Whatever is left on an order that cannot rest is cancelled here, so the
    // caller's `leaves_qty.is_zero()` test means "done", not "fully filled".
    if !rests(order) {
        working.leaves_qty = Qty::ZERO;
    }
    Ok(())
}

/// Whether an order can trade at `price`.
///
/// A market order crosses everything; a limit order crosses at its limit or
/// better.
fn crosses(order: &Order, price: Px) -> bool {
    match order.price {
        None => true,
        Some(limit) => match order.side {
            Side::Bid => price <= limit,
            Side::Ask => price >= limit,
        },
    }
}

/// The simulated venue, wired against a maintained book.
///
/// Not in the [`prelude`](crate::prelude) — `use
/// wingfoil::adapters::execution::sim::SimVenueOps;`.
pub trait SimVenueOps {
    /// Match `orders` against this book, emitting the fills.
    ///
    /// Ticks on any cycle that produces at least one fill, and stays quiet
    /// otherwise. Orders match the book **as of the previous instant** — see
    /// the module docs, decision 1.
    ///
    /// The `orders` stream is normally the receiving half of a
    /// [`feedback`](crate::fluent::StreamOps::feedback) edge, which is what
    /// makes the strategy → order → fill → strategy loop a DAG and imposes the
    /// one-nanosecond causality floor on the order hop.
    fn sim_venue(&self, orders: &Stream<Burst<Order>>) -> Stream<Burst<Fill>>;
}

impl SimVenueOps for Stream<Arc<OrderBook>> {
    fn sim_venue(&self, orders: &Stream<Burst<Order>>) -> Stream<Burst<Fill>> {
        let orders = orders.handle();
        self.wire(|b, h| b.sim_venue(h, orders))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::NanoTime;
    use crate::adapters::market::{BookSnapshot, BookUpdate, InstrumentId, Level, Sequencing};

    fn inst() -> InstrumentId {
        InstrumentId::new("test", "BTC-USD")
    }

    /// A two-deep book: bids at 100 (×2) and 99 (×5), asks at 101 (×3) and
    /// 102 (×4).
    fn book() -> OrderBook {
        let mut book = OrderBook::new(inst());
        book.apply(&BookUpdate::Snapshot(BookSnapshot {
            instrument: inst(),
            bids: vec![
                Level::new(Px::parse("100").unwrap(), Qty::parse("2").unwrap()),
                Level::new(Px::parse("99").unwrap(), Qty::parse("5").unwrap()),
            ],
            asks: vec![
                Level::new(Px::parse("101").unwrap(), Qty::parse("3").unwrap()),
                Level::new(Px::parse("102").unwrap(), Qty::parse("4").unwrap()),
            ],
            sequencing: Sequencing::Single(1),
            venue_time: None,
            recv_time: NanoTime::ZERO,
        }));
        book
    }

    fn order(side: Side, qty: &str, price: Option<&str>) -> Order {
        let qty = Qty::parse(qty).unwrap();
        match price {
            Some(p) => Order::limit(ClOrdId::new("o1"), inst(), side, qty, Px::parse(p).unwrap()),
            None => Order::market(ClOrdId::new("o1"), inst(), side, qty),
        }
    }

    /// Run one order through a fresh simulator against `book()`.
    fn matched(order: Order) -> (Burst<Fill>, SimVenueState) {
        let mut state = SimVenueState::default();
        let mut fills = Burst::new();
        let mut working = Working {
            cum_qty: Qty::ZERO,
            leaves_qty: order.qty,
            order,
        };
        match_one(
            &mut state,
            &mut working,
            &book(),
            NanoTime::ZERO,
            &mut fills,
        )
        .unwrap();
        if !working.leaves_qty.is_zero() && rests(&working.order) {
            state.working.push(working);
        }
        (fills, state)
    }

    #[test]
    fn a_marketable_limit_takes_the_touch() {
        let (fills, _) = matched(order(Side::Bid, "1", Some("101")));
        assert_eq!(fills.len(), 1);
        assert_eq!(fills[0].price, Px::parse("101").unwrap());
        assert_eq!(fills[0].qty, Qty::parse("1").unwrap());
        assert_eq!(fills[0].leaves_qty, Qty::ZERO);
    }

    #[test]
    fn a_passive_limit_does_not_fill_and_rests() {
        let (fills, state) = matched(order(Side::Bid, "1", Some("100")));
        assert!(fills.is_empty());
        assert_eq!(state.working_orders(), 1);
    }

    #[test]
    fn crossing_several_levels_yields_several_fills_in_one_burst() {
        let (fills, _) = matched(order(Side::Bid, "5", Some("102")));
        assert_eq!(fills.len(), 2);
        assert_eq!(fills[0].price, Px::parse("101").unwrap());
        assert_eq!(fills[0].qty, Qty::parse("3").unwrap());
        assert_eq!(fills[0].cum_qty, Qty::parse("3").unwrap());
        assert_eq!(fills[0].leaves_qty, Qty::parse("2").unwrap());
        assert_eq!(fills[1].price, Px::parse("102").unwrap());
        assert_eq!(fills[1].qty, Qty::parse("2").unwrap());
        assert_eq!(fills[1].cum_qty, Qty::parse("5").unwrap());
        assert_eq!(fills[1].leaves_qty, Qty::ZERO);
    }

    #[test]
    fn a_limit_stops_at_the_first_level_it_cannot_reach() {
        // Wants 5 but only crosses the 101 level's 3.
        let (fills, state) = matched(order(Side::Bid, "5", Some("101")));
        assert_eq!(fills.len(), 1);
        assert_eq!(fills[0].leaves_qty, Qty::parse("2").unwrap());
        // The unfilled remainder rests.
        assert_eq!(state.working_orders(), 1);
    }

    #[test]
    fn a_market_order_walks_the_side_and_never_rests() {
        let (fills, state) = matched(order(Side::Ask, "9", None));
        assert_eq!(fills.len(), 2);
        assert_eq!(fills[0].price, Px::parse("100").unwrap());
        assert_eq!(fills[1].price, Px::parse("99").unwrap());
        // Only seven were available; the remaining two are cancelled, not
        // rested, and the last fill reports the order as done.
        assert_eq!(fills[1].leaves_qty, Qty::parse("2").unwrap());
        assert_eq!(state.working_orders(), 0);
    }

    #[test]
    fn immediate_or_cancel_takes_what_it_can_and_stops() {
        let (fills, state) = matched(
            order(Side::Bid, "5", Some("101")).with_time_in_force(TimeInForce::ImmediateOrCancel),
        );
        assert_eq!(fills.len(), 1);
        assert_eq!(fills[0].qty, Qty::parse("3").unwrap());
        assert_eq!(state.working_orders(), 0);
    }

    #[test]
    fn fill_or_kill_that_cannot_complete_fills_nothing() {
        let (fills, state) =
            matched(order(Side::Bid, "5", Some("101")).with_time_in_force(TimeInForce::FillOrKill));
        assert!(fills.is_empty());
        assert_eq!(state.working_orders(), 0);
    }

    #[test]
    fn fill_or_kill_that_can_complete_fills_in_full() {
        let (fills, _) =
            matched(order(Side::Bid, "5", Some("102")).with_time_in_force(TimeInForce::FillOrKill));
        assert_eq!(fills.len(), 2);
        assert_eq!(fills[1].leaves_qty, Qty::ZERO);
    }

    #[test]
    fn a_sell_crosses_downwards_into_the_bids() {
        let (fills, _) = matched(order(Side::Ask, "3", Some("99")));
        assert_eq!(fills.len(), 2);
        assert_eq!(fills[0].price, Px::parse("100").unwrap());
        assert_eq!(fills[0].qty, Qty::parse("2").unwrap());
        assert_eq!(fills[1].price, Px::parse("99").unwrap());
        assert_eq!(fills[1].qty, Qty::parse("1").unwrap());
    }

    #[test]
    fn a_gapped_book_fills_nothing() {
        // `depth` returns nothing while the book is not live, so an order
        // against one simply does not trade — the conservative answer.
        let empty = OrderBook::new(inst());
        let mut state = SimVenueState::default();
        let mut fills = Burst::new();
        let mut working = Working {
            cum_qty: Qty::ZERO,
            leaves_qty: Qty::parse("1").unwrap(),
            order: order(Side::Bid, "1", None),
        };
        match_one(&mut state, &mut working, &empty, NanoTime::ZERO, &mut fills).unwrap();
        assert!(fills.is_empty());
    }

    #[test]
    fn execution_ids_are_unique_across_a_burst() {
        let (fills, _) = matched(order(Side::Bid, "5", Some("102")));
        assert_ne!(fills[0].exec_id, fills[1].exec_id);
    }
}
