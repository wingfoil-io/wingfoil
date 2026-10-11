//! The client-order desk: browser clicks in, OMS decisions out, fills
//! allocated back to the clicks that asked for them.
//!
//! The execution layer's OMS is *state-driven*: it is told what the
//! strategy wants showing on an instrument — a [`Desired`] — and works out
//! the requests that get there, one in flight per side. The browser is
//! *order-driven*: every click is an order of its own and wants its own
//! answer. The desk is the layer between the two, which every agency
//! gateway has in some form:
//!
//! - **Pre-trade checks.** A click on a stale book, over the per-order size,
//!   past the position limit, or while the kill switch is latched is
//!   answered at once with a zero fill. Nothing it would have sent reaches
//!   the OMS.
//! - **Aggregation.** The clicks waiting on a side are one decision: a cross
//!   (an immediate-or-cancel limit, capped at the far touch) for their
//!   total, up to the per-order size. Two users buying in the same
//!   instant are one order at the venue.
//! - **Allocation.** When the OMS places that order the desk learns which
//!   clicks it carried; each execution on it is handed out to those clicks
//!   first-in first-out, and when the order ends — filled out, or the IOC's
//!   rest cancelled — every click on it is answered with what it got.
//!
//! The desk is a plain struct, driven by the graph in a fixed order each
//! instant (requests, reports, risk, sweep, book, clicks — see `fix_gw`),
//! and tested below without one.

use std::collections::{HashMap, VecDeque};
use std::time::Duration;

use wingfoil::Burst;
use wingfoil::NanoTime;
use wingfoil::adapters::execution::edge::{RejectReason, Report, Request};
use wingfoil::adapters::execution::oms::Desired;
use wingfoil::adapters::execution::order::ClientOrderId;
use wingfoil::adapters::market::{Amount, Level, Px, Qty, SCALE, Side, mul_div};
use wingfoil::latency::{Stage, Stamping, Traced};

use crate::lmax::EurUsd;
use crate::shared::{RoundTrip, RoundTripLatency, SIDE_BUY, round_trip_latency};

/// One browser order, with its latency record.
pub type Click = Traced<RoundTrip, RoundTripLatency>;

/// The desk's pre-trade limits.
#[derive(Clone, Copy, Debug)]
pub struct Limits {
    /// The largest single click, and the largest order the desk will ask
    /// the OMS for — the same number as the venue ceiling, so the ceiling
    /// is a backstop the desk never reaches.
    pub max_order: Qty,
    /// The largest net position, either way, the desk will trade into.
    pub max_position: Qty,
    /// How old the top of book may be for a click to be priced against it.
    pub max_md_age: Duration,
    /// How long a click may wait unplaced — behind the order-rate budget,
    /// say — before it is answered unfilled. Inside the OMS's
    /// `max_desired_age`, so the desk gives up before the OMS withdraws.
    pub max_wait: Duration,
    /// How long a placed order may go without ending before its clicks are
    /// answered with what they got. An IOC ends in milliseconds; one that
    /// has not ended in this long has lost its report — a dropped session,
    /// a refusal the desk could not attribute — and its clicks are not left
    /// waiting on it forever.
    pub max_working: Duration,
}

/// The touch, as the market-data session last stated it.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Touch {
    pub bid: Option<Px>,
    pub ask: Option<Px>,
    /// Engine time of the last update.
    pub at: NanoTime,
}

impl Touch {
    /// The mid, where both sides are known.
    pub fn mid(&self) -> Option<Px> {
        Some(Px::midpoint(self.bid?, self.ask?))
    }
}

/// What the risk node last said: the position, and whether the kill switch
/// is latched.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct RiskView {
    pub net: Qty,
    pub halted: bool,
}

/// A click carried by a placed order.
#[derive(Debug)]
struct Alloc {
    click: Click,
    want: Qty,
    filled: Qty,
    /// Σ qty × price over its executions, for the average price the
    /// browser shows — exact, in `market`'s fixed point.
    notional: Amount,
}

/// A placed order and the clicks it carries, in arrival order.
#[derive(Debug)]
struct Working {
    side: Side,
    /// The order's size, and how much of it has filled — the order's own,
    /// not its clicks': what the position check counts as still to come.
    qty: Qty,
    filled: Qty,
    /// Its clicks were answered after `max_working` with no end reported.
    /// It stays — its side busy, its rest counted — until one is, because
    /// the OMS's slot and the venue may still be holding it.
    abandoned: bool,
    allocs: Vec<Alloc>,
    /// A cancel is in flight on it, so a reject may name the cancel rather
    /// than the order.
    cancelling: bool,
    /// When the desk learned it was placed.
    at: NanoTime,
}

pub struct Desk {
    limits: Limits,
    stamping: Stamping,
    touch: Touch,
    risk: RiskView,
    /// Clicks not yet on an order, per side ([`slot`]).
    pending: [VecDeque<(Click, Qty, NanoTime)>; 2],
    working: HashMap<ClientOrderId, Working>,
    /// Whether this instant changed what is pending, so a fresh decision is
    /// owed to the OMS.
    dirty: bool,
}

/// Index of a side in [`Desk::pending`].
fn slot(side: Side) -> usize {
    match side {
        Side::Bid => 0,
        Side::Ask => 1,
    }
}

fn side_of(click: &Click) -> Side {
    if click.payload.side == SIDE_BUY {
        Side::Bid
    } else {
        Side::Ask
    }
}

fn signed(side: Side, qty: Qty) -> Qty {
    match side {
        Side::Bid => qty,
        Side::Ask => -qty,
    }
}

impl Desk {
    pub fn new(limits: Limits, stamping: Stamping) -> Desk {
        Desk {
            limits,
            stamping,
            touch: Touch::default(),
            risk: RiskView::default(),
            pending: [VecDeque::new(), VecDeque::new()],
            working: HashMap::new(),
            dirty: false,
        }
    }

    fn stamp<S: Stage<RoundTripLatency>>(&self, click: &mut Click) {
        if self.stamping.is_on() {
            S::stamp(&mut click.latency, u64::from(NanoTime::now()));
        }
    }

    /// What the OMS sent last instant — the feedback edge's burst, the same
    /// one the venue just rendered, arriving at `now`. A place carries the
    /// oldest clicks waiting on its side, as many whole ones as its size
    /// holds.
    pub fn sent(&mut self, now: NanoTime, requests: &[Request<EurUsd>]) {
        for request in requests {
            match *request {
                Request::Place(order) => {
                    let mut allocs = Vec::new();
                    let mut carried = Qty::ZERO;
                    let pending = &mut self.pending[slot(order.side)];
                    while let Some((_, want, _)) = pending.front() {
                        // At least the front click, even if it is larger
                        // than the order — a decision restated after some
                        // of its clicks were answered — so what the order
                        // fills always reaches a click.
                        if !allocs.is_empty() && carried + *want > order.qty {
                            break;
                        }
                        let (mut click, want, _) = pending.pop_front().expect("front exists");
                        carried = carried + want;
                        if self.stamping.is_on() {
                            round_trip_latency::fix_send::stamp(
                                &mut click.latency,
                                u64::from(NanoTime::now()),
                            );
                        }
                        allocs.push(Alloc {
                            click,
                            want,
                            filled: Qty::ZERO,
                            notional: Amount::ZERO,
                        });
                    }
                    if carried < order.qty {
                        // The OMS crossed a decision whose clicks the desk
                        // had already answered. What fills past the clicks
                        // still moves the position — the fold books every
                        // execution, and the desk counts it as to come —
                        // but is allocated to no click.
                        log::error!(
                            "desk: order {:?} for {} carries clicks for only {carried}",
                            order.id,
                            order.qty
                        );
                    }
                    log::info!(
                        "desk: order {} {:?} {} @ {:?} carries {} click(s)",
                        order.id.sequence(),
                        order.side,
                        order.qty,
                        order.price,
                        allocs.len()
                    );
                    self.working.insert(
                        order.id,
                        Working {
                            side: order.side,
                            qty: order.qty,
                            filled: Qty::ZERO,
                            abandoned: false,
                            allocs,
                            cancelling: false,
                            at: now,
                        },
                    );
                    // Anything still waiting, either side, is the next
                    // decision.
                    if self.waiting() {
                        self.dirty = true;
                    }
                }
                Request::Cancel(id) => {
                    if let Some(working) = self.working.get_mut(&id) {
                        working.cancelling = true;
                    }
                }
                Request::CancelAll => {
                    for working in self.working.values_mut() {
                        working.cancelling = true;
                    }
                }
                Request::Amend(_) => {}
            }
        }
    }

    /// What the venue said. Executions are allocated; an order's end
    /// answers every click on it.
    pub fn reported(&mut self, reports: &[Report<EurUsd>], out: &mut Burst<Click>) {
        for report in reports {
            let (id, ended) = match report {
                Report::Fill(fill) => {
                    let Some(working) = self.working.get_mut(&fill.order) else {
                        continue;
                    };
                    working.filled = working.filled + fill.qty;
                    let mut left = fill.qty;
                    for alloc in &mut working.allocs {
                        let take = std::cmp::min(left, alloc.want - alloc.filled);
                        if take <= Qty::ZERO {
                            continue;
                        }
                        alloc.filled = alloc.filled + take;
                        if let Some(notional) = Amount::of(take, fill.price) {
                            alloc.notional = alloc.notional + notional;
                        }
                        left = left - take;
                    }
                    (fill.order, fill.remaining <= Qty::ZERO)
                }
                Report::Cancelled(retired) | Report::Expired(retired) => (retired.order, true),
                Report::Reject(reject) => {
                    let Some(id) = reject.order else { continue };
                    match self.working.get_mut(&id) {
                        // With a cancel in flight a refusal is the cancel's,
                        // and the order still lives — unless the venue says
                        // the order is gone, which ends it whatever was
                        // refused. A refusal of the order itself under a
                        // cancel cannot be told from the cancel's here;
                        // `max_working` is what answers that one.
                        Some(working)
                            if working.cancelling
                                && reject.reason != RejectReason::UnknownOrder =>
                        {
                            working.cancelling = false;
                            (id, false)
                        }
                        _ => (id, true),
                    }
                }
                Report::Ack(_) | Report::Triggered(_) => continue,
            };
            if ended && let Some(working) = self.working.remove(&id) {
                answer_all(working, out);
                // The side is free: restate what still waits.
                if self.waiting() {
                    self.dirty = true;
                }
            }
        }
    }

    /// The risk node's latest word. A latch answers everything still
    /// waiting, unfilled; what is already at the venue is the OMS's
    /// cancel-all's to pull.
    pub fn risk(&mut self, risk: RiskView, out: &mut Burst<Click>) {
        self.risk = risk;
        if risk.halted {
            for pending in &mut self.pending {
                for (click, _, _) in pending.drain(..) {
                    out.push(answer(click, Qty::ZERO, Amount::ZERO));
                    self.dirty = true;
                }
            }
        }
    }

    /// The clock: answer clicks that have waited past `max_wait` unplaced,
    /// and those on an order that has gone `max_working` without ending.
    pub fn sweep(&mut self, now: NanoTime, out: &mut Burst<Click>) {
        for (id, working) in &mut self.working {
            let overdue =
                now >= working.at && Duration::from(now - working.at) >= self.limits.max_working;
            if working.abandoned || !overdue {
                continue;
            }
            log::error!(
                "desk: order {id:?} has not ended in {:?}; answering its clicks with what \
                 they got, and counting its rest until it does",
                self.limits.max_working
            );
            working.abandoned = true;
            for alloc in working.allocs.drain(..) {
                out.push(answer(alloc.click, alloc.filled, alloc.notional));
            }
        }
        for pending in &mut self.pending {
            while let Some((_, _, at)) = pending.front() {
                if Duration::from(now - *at) < self.limits.max_wait {
                    break;
                }
                let (click, _, _) = pending.pop_front().expect("front exists");
                log::warn!(
                    "desk: click seq={} waited too long",
                    click.payload.client_seq
                );
                out.push(answer(click, Qty::ZERO, Amount::ZERO));
                self.dirty = true;
            }
        }
    }

    pub fn touch(&mut self, touch: Touch) {
        self.touch = touch;
    }

    /// A new click: pre-trade checks, then onto its side's queue.
    pub fn click(&mut self, now: NanoTime, mut click: Click, out: &mut Burst<Click>) {
        self.stamp::<round_trip_latency::gw_price>(&mut click);
        let side = side_of(&click);
        let seq = click.payload.client_seq;
        let refuse = |why: &str, out: &mut Burst<Click>, click: Click| {
            log::warn!("desk: click seq={seq} refused — {why}");
            out.push(answer(click, Qty::ZERO, Amount::ZERO));
        };
        let Ok(qty) = Qty::parse(&click.payload.qty.to_string()) else {
            return refuse("unreadable quantity", out, click);
        };
        if qty <= Qty::ZERO || qty > self.limits.max_order {
            return refuse("size outside the per-order limit", out, click);
        }
        if self.risk.halted {
            return refuse("kill switch latched", out, click);
        }
        let fresh = self.touch.bid.is_some()
            && self.touch.ask.is_some()
            && now >= self.touch.at
            && Duration::from(now - self.touch.at) <= self.limits.max_md_age;
        if !fresh {
            return refuse("book stale or empty", out, click);
        }
        // Judged against the worst case on the click's own side: every
        // order and waiting click that way filling while nothing the other
        // way does. Netting the two would let a buy in on the strength of a
        // sell that may yet be killed unfilled.
        let worst = self.risk.net + self.outstanding(side) + signed(side, qty);
        let limit = match side {
            Side::Bid => worst > self.limits.max_position,
            Side::Ask => worst < -self.limits.max_position,
        };
        if limit {
            return refuse("position limit", out, click);
        }
        self.pending[slot(side)].push_back((click, qty, now));
        self.dirty = true;
    }

    /// What could still fill on `side`, signed: the unfilled rest of every
    /// order that way — the order's own size, whatever clicks it carries,
    /// and an abandoned order's too — and every click waiting that way.
    fn outstanding(&self, side: Side) -> Qty {
        let mut total = Qty::ZERO;
        for working in self.working.values().filter(|w| w.side == side) {
            if working.qty > working.filled {
                total = total + signed(side, working.qty - working.filled);
            }
        }
        for (_, qty, _) in &self.pending[slot(side)] {
            total = total + signed(side, *qty);
        }
        total
    }

    /// The decision this instant owes the OMS, if what is waiting changed:
    /// a cross to the far touch — a buy capped at the ask, a sell at the
    /// bid — for the oldest whole clicks on one side that fit in one order,
    /// or nothing.
    ///
    /// One side a decision, because a cross is one level on one side
    /// ([`Desired::cross`]), and the OMS holds one plain decision an
    /// instrument. A side with no order in flight goes before one that has
    /// — the OMS could not send the busy side's yet, and stating it would
    /// leave the free side's clicks waiting behind it — and among those
    /// alike, the side whose oldest click has waited longest. The other is
    /// decided as soon as that order is placed or ends.
    pub fn decide(&mut self, now: NanoTime) -> Option<Desired<EurUsd>> {
        if !std::mem::take(&mut self.dirty) {
            return None;
        }
        let busy = |side: Side| self.working.values().any(|w| w.side == side);
        let first = [Side::Bid, Side::Ask]
            .into_iter()
            .filter_map(|side| Some((busy(side), self.pending[slot(side)].front()?.2, side)))
            .min();
        let Some((_, _, side)) = first else {
            return Some(Desired::nothing(EurUsd, now));
        };
        let cap = match side {
            Side::Bid => self.touch.ask,
            Side::Ask => self.touch.bid,
        };
        let mut total = Qty::ZERO;
        for (_, qty, _) in &self.pending[slot(side)] {
            if total + *qty > self.limits.max_order {
                break;
            }
            total = total + *qty;
        }
        Some(match cap {
            Some(cap) => Desired::cross(EurUsd, now, side, Level::new(cap, total)),
            None => Desired::nothing(EurUsd, now),
        })
    }

    fn waiting(&self) -> bool {
        self.pending.iter().any(|pending| !pending.is_empty())
    }
}

/// Answer every click on a finished (or given-up-on) order.
fn answer_all(working: Working, out: &mut Burst<Click>) {
    for alloc in working.allocs {
        out.push(answer(alloc.click, alloc.filled, alloc.notional));
    }
}

/// A click's answer: what it got, at its average price in the wire's
/// basis points. The average is exact (`Amount::per`); only the last step,
/// into the wire's integer types, rounds.
fn answer(mut click: Click, filled: Qty, notional: Amount) -> Click {
    click.payload.filled_qty = u64::try_from(filled.raw() / SCALE).unwrap_or(0);
    // Twice the basis points, then halved rounding half up: `mul_div`
    // truncates, which would show every price up to a basis point low.
    click.payload.fill_price_bps = notional
        .per(filled)
        .and_then(|avg| mul_div(avg.raw(), 20_000, SCALE))
        .and_then(|twice| i64::try_from((twice + 1) / 2).ok())
        .unwrap_or(0);
    click
}

#[cfg(test)]
mod tests {
    use super::*;
    use wingfoil::adapters::execution::edge::Retired;
    use wingfoil::adapters::execution::exec_id::ExecId;
    use wingfoil::adapters::execution::order::{Epoch, Fill, Order};
    use wingfoil::adapters::market::Px;

    fn qty(n: &str) -> Qty {
        Qty::parse(n).unwrap()
    }

    fn px(p: &str) -> Px {
        Px::parse(p).unwrap()
    }

    fn t(ms: u64) -> NanoTime {
        NanoTime::new(1_000_000_000 + ms * 1_000_000)
    }

    fn desk() -> Desk {
        let mut desk = Desk::new(
            Limits {
                max_order: qty("10"),
                max_position: qty("20"),
                max_md_age: Duration::from_secs(60),
                max_wait: Duration::from_secs(2),
                max_working: Duration::from_secs(10),
            },
            Stamping::Off,
        );
        desk.touch(Touch {
            bid: Some(px("1.0850")),
            ask: Some(px("1.0852")),
            at: t(0),
        });
        desk
    }

    fn click(seq: u64, side: u8, qty: u64) -> Click {
        Click::new(RoundTrip {
            client_seq: seq,
            side,
            qty,
            ..RoundTrip::default()
        })
    }

    fn id(n: u32) -> ClientOrderId {
        ClientOrderId::new(Epoch::ZERO, n)
    }

    fn fill(order: u32, q: &str, p: &str, remaining: &str) -> Report<EurUsd> {
        Report::Fill(Fill {
            order: id(order),
            exec_id: ExecId::new("x").unwrap(),
            instrument: EurUsd,
            side: Side::Bid,
            qty: qty(q),
            price: px(p),
            remaining: qty(remaining),
            ..Default::default()
        })
    }

    #[test]
    fn two_buys_in_one_instant_are_one_cross_capped_at_the_ask() {
        let mut desk = desk();
        let mut out = Burst::new();
        desk.click(t(1), click(1, SIDE_BUY, 2), &mut out);
        desk.click(t(1), click(2, SIDE_BUY, 3), &mut out);
        assert!(out.is_empty());
        let desired = desk.decide(t(1)).unwrap();
        assert_eq!(
            desired.intent,
            wingfoil::adapters::execution::oms::Intent::Cross
        );
        assert_eq!(
            desired.bids.best(),
            Some(Level::new(px("1.0852"), qty("5")))
        );
        assert!(desired.asks.is_empty());
        assert_eq!(desk.decide(t(1)), None, "nothing new, nothing owed");
    }

    #[test]
    fn a_buy_and_a_sell_are_crossed_one_side_at_a_time_oldest_first() {
        let mut desk = desk();
        let mut out = Burst::new();
        desk.click(t(1), click(1, crate::shared::SIDE_SELL, 2), &mut out);
        desk.click(t(2), click(2, SIDE_BUY, 3), &mut out);
        let first = desk.decide(t(2)).unwrap();
        assert_eq!(first.asks.best(), Some(Level::new(px("1.0850"), qty("2"))));
        assert!(first.bids.is_empty());
        desk.sent(
            t(1),
            &[Request::Place(Order::limit(
                id(1),
                EurUsd,
                Side::Ask,
                qty("2"),
                px("1.0850"),
            ))],
        );
        let second = desk.decide(t(3)).unwrap();
        assert_eq!(second.bids.best(), Some(Level::new(px("1.0852"), qty("3"))));
        assert!(second.asks.is_empty());
    }

    #[test]
    fn fills_are_allocated_first_in_first_out_and_answered_when_the_order_ends() {
        let mut desk = desk();
        let mut out = Burst::new();
        desk.click(t(1), click(1, SIDE_BUY, 2), &mut out);
        desk.click(t(1), click(2, SIDE_BUY, 3), &mut out);
        desk.decide(t(1));
        desk.sent(
            t(1),
            &[Request::Place(Order::limit(
                id(1),
                EurUsd,
                Side::Bid,
                qty("5"),
                px("1.0852"),
            ))],
        );
        desk.reported(&[fill(1, "3", "1.0851", "2")], &mut out);
        assert!(out.is_empty(), "the IOC has not ended");
        desk.reported(
            &[Report::Cancelled(Retired {
                order: id(1),
                remaining: qty("2"),
                ..Default::default()
            })],
            &mut out,
        );
        let answered: Vec<(u64, u64, i64)> = out
            .iter()
            .map(|c| {
                (
                    c.payload.client_seq,
                    c.payload.filled_qty,
                    c.payload.fill_price_bps,
                )
            })
            .collect();
        assert_eq!(answered, vec![(1, 2, 10851), (2, 1, 10851)]);
    }

    #[test]
    fn a_click_past_the_position_limit_is_refused_but_one_that_reduces_is_not() {
        let mut desk = desk();
        let mut out = Burst::new();
        desk.risk(
            RiskView {
                net: qty("18"),
                halted: false,
            },
            &mut out,
        );
        desk.click(t(1), click(1, SIDE_BUY, 3), &mut out);
        assert_eq!(out.len(), 1, "refused at once");
        assert_eq!(out[0].payload.filled_qty, 0);
        desk.click(t(1), click(2, crate::shared::SIDE_SELL, 3), &mut out);
        assert_eq!(out.len(), 1, "a sell reduces and is taken");
    }

    #[test]
    fn a_latch_answers_everything_waiting_and_refuses_what_comes_next() {
        let mut desk = desk();
        let mut out = Burst::new();
        desk.click(t(1), click(1, SIDE_BUY, 1), &mut out);
        desk.risk(
            RiskView {
                net: Qty::ZERO,
                halted: true,
            },
            &mut out,
        );
        desk.click(t(2), click(2, SIDE_BUY, 1), &mut out);
        assert_eq!(out.len(), 2);
        let desired = desk.decide(t(2)).unwrap();
        assert!(desired.is_empty(), "withdraws both sides: {desired:?}");
    }

    fn place(n: u32, side: Side, size: &str, at: NanoTime, desk: &mut Desk) {
        desk.sent(
            at,
            &[Request::Place(Order::limit(
                id(n),
                EurUsd,
                side,
                qty(size),
                px("1.0851"),
            ))],
        );
    }

    /// The position check takes the worst case on the click's own side: a
    /// sell waiting to be crossed does not make room for a buy, because the
    /// sell's IOC may be killed unfilled while the buy fills.
    #[test]
    fn a_sell_in_flight_does_not_make_room_for_a_buy() {
        let mut desk = desk();
        let mut out = Burst::new();
        desk.risk(
            RiskView {
                net: qty("15"),
                halted: false,
            },
            &mut out,
        );
        desk.click(t(1), click(1, crate::shared::SIDE_SELL, 10), &mut out);
        assert!(out.is_empty());
        desk.click(t(1), click(2, SIDE_BUY, 10), &mut out);
        assert_eq!(
            out.len(),
            1,
            "refused: 15 + 10 is past 20 whatever the sell does"
        );
        assert_eq!(out[0].payload.client_seq, 2);
    }

    /// A refusal that says the order is gone ends it, cancel in flight or
    /// not — the venue answered a cancel of an order it no longer has.
    #[test]
    fn an_unknown_order_refusal_ends_the_order_under_a_cancel() {
        let mut desk = desk();
        let mut out = Burst::new();
        desk.click(t(1), click(1, SIDE_BUY, 2), &mut out);
        desk.decide(t(1));
        place(1, Side::Bid, "2", t(2), &mut desk);
        desk.sent(t(3), &[Request::CancelAll]);
        let refused = |reason| {
            Report::Reject(wingfoil::adapters::execution::edge::Reject {
                order: Some(id(1)),
                reason,
                venue_time: None,
                recv_time: t(4),
            })
        };
        desk.reported(&[refused(RejectReason::Other)], &mut out);
        assert!(out.is_empty(), "a refused cancel leaves the order working");
        desk.sent(t(5), &[Request::CancelAll]);
        desk.reported(&[refused(RejectReason::UnknownOrder)], &mut out);
        assert_eq!(out.len(), 1, "gone: its click is answered");
    }

    /// An order that never ends — its report lost — has its clicks
    /// answered after `max_working`, not held forever.
    #[test]
    fn an_order_that_never_ends_is_given_up_on() {
        let mut desk = desk();
        let mut out = Burst::new();
        desk.click(t(1), click(1, SIDE_BUY, 2), &mut out);
        desk.decide(t(1));
        place(1, Side::Bid, "2", t(2), &mut desk);
        desk.sweep(t(9_000), &mut out);
        assert!(out.is_empty());
        desk.sweep(t(10_002), &mut out);
        assert_eq!(out.len(), 1);
        assert_eq!(out[0].payload.filled_qty, 0);
        desk.sweep(t(20_000), &mut out);
        assert_eq!(out.len(), 1, "answered once");
        // The order is still counted: a buy that would take the worst case
        // past 20 with its 2 still to come is refused.
        desk.risk(
            RiskView {
                net: qty("10"),
                halted: false,
            },
            &mut out,
        );
        desk.click(t(20_001), click(2, SIDE_BUY, 9), &mut out);
        assert_eq!(out.len(), 2, "10 + 2 + 9 is past 20");
        // When it does end, it is gone, and nothing is answered twice.
        desk.reported(
            &[Report::Cancelled(Retired {
                order: id(1),
                remaining: qty("2"),
                ..Default::default()
            })],
            &mut out,
        );
        assert_eq!(out.len(), 2);
        desk.click(t(20_002), click(3, SIDE_BUY, 9), &mut out);
        assert_eq!(out.len(), 2, "10 + 9 is inside 20");
    }

    /// A place smaller than the click at the front of the queue still
    /// carries it, so what the order fills reaches a click.
    #[test]
    fn a_place_smaller_than_its_front_click_still_carries_it() {
        let mut desk = desk();
        let mut out = Burst::new();
        desk.click(t(1), click(1, SIDE_BUY, 3), &mut out);
        desk.decide(t(1));
        place(1, Side::Bid, "2", t(2), &mut desk);
        desk.reported(&[fill(1, "2", "1.0851", "0")], &mut out);
        assert_eq!(out.len(), 1);
        assert_eq!(out[0].payload.filled_qty, 2);
    }

    /// The wire's basis points round to nearest, never down.
    #[test]
    fn the_average_price_rounds_to_the_nearest_basis_point() {
        let price = |p: &str| {
            let notional = Amount::of(qty("1"), px(p)).unwrap();
            answer(click(1, SIDE_BUY, 1), qty("1"), notional)
                .payload
                .fill_price_bps
        };
        assert_eq!(price("1.08515"), 10852);
        assert_eq!(price("1.08514"), 10851);
    }

    /// A side with an order in flight does not hold up the other: the
    /// free side is decided first, whatever waited longest.
    #[test]
    fn a_busy_side_does_not_starve_the_free_one() {
        let mut desk = desk();
        let mut out = Burst::new();
        desk.click(t(1), click(1, SIDE_BUY, 2), &mut out);
        desk.decide(t(1));
        place(1, Side::Bid, "2", t(2), &mut desk);
        desk.click(t(3), click(2, SIDE_BUY, 1), &mut out);
        desk.click(t(4), click(3, crate::shared::SIDE_SELL, 1), &mut out);
        let desired = desk.decide(t(4)).unwrap();
        assert!(desired.bids.is_empty(), "{desired:?}");
        assert_eq!(
            desired.asks.best(),
            Some(Level::new(px("1.0850"), qty("1")))
        );
    }

    #[test]
    fn a_stale_book_refuses_the_click() {
        let mut desk = desk();
        let mut out = Burst::new();
        desk.click(t(61_000), click(1, SIDE_BUY, 1), &mut out);
        assert_eq!(out.len(), 1);
    }

    #[test]
    fn a_click_that_waits_too_long_is_answered_unfilled() {
        let mut desk = desk();
        let mut out = Burst::new();
        desk.click(t(1), click(1, SIDE_BUY, 1), &mut out);
        desk.sweep(t(1_000), &mut out);
        assert!(out.is_empty());
        desk.sweep(t(2_001), &mut out);
        assert_eq!(out.len(), 1);
    }
}
