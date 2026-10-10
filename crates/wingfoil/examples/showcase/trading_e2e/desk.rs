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
use wingfoil::adapters::execution::edge::{Report, Request};
use wingfoil::adapters::execution::oms::{Desired, Intent, Ladder};
use wingfoil::adapters::execution::order::ClientOrderId;
use wingfoil::adapters::market::{Level, Px, Qty, Side};
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
    /// browser shows. Presentation only, hence `f64`.
    notional: f64,
}

/// A placed order and the clicks it carries, in arrival order.
#[derive(Debug)]
struct Working {
    side: Side,
    allocs: Vec<Alloc>,
    /// A cancel is in flight on it, so a reject names the cancel, not the
    /// order.
    cancelling: bool,
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
    /// one the venue just rendered. A place carries the oldest clicks
    /// waiting on its side, as many whole ones as its size holds.
    pub fn sent(&mut self, requests: &[Request<EurUsd>]) {
        for request in requests {
            match *request {
                Request::Place(order) => {
                    let mut allocs = Vec::new();
                    let mut carried = Qty::ZERO;
                    let pending = &mut self.pending[slot(order.side)];
                    while let Some((_, want, _)) = pending.front() {
                        if carried + *want > order.qty {
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
                            notional: 0.0,
                        });
                    }
                    if carried != order.qty {
                        log::warn!(
                            "desk: order {:?} for {} carries clicks for {carried}",
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
                            allocs,
                            cancelling: false,
                        },
                    );
                    // Anything left waiting is a new decision for the rest.
                    if !self.pending[slot(order.side)].is_empty() {
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
                    let mut left = fill.qty;
                    for alloc in &mut working.allocs {
                        let take = std::cmp::min(left, alloc.want - alloc.filled);
                        if take <= Qty::ZERO {
                            continue;
                        }
                        alloc.filled = alloc.filled + take;
                        alloc.notional += take.to_f64() * fill.price.to_f64();
                        left = left - take;
                    }
                    (fill.order, fill.remaining <= Qty::ZERO)
                }
                Report::Cancelled(retired) | Report::Expired(retired) => (retired.order, true),
                Report::Reject(reject) => {
                    let Some(id) = reject.order else { continue };
                    match self.working.get_mut(&id) {
                        // A refused cancel: the order still lives.
                        Some(working) if working.cancelling => {
                            working.cancelling = false;
                            (id, false)
                        }
                        _ => (id, true),
                    }
                }
                Report::Ack(_) | Report::Triggered(_) => continue,
            };
            if ended && let Some(working) = self.working.remove(&id) {
                for alloc in working.allocs {
                    out.push(answer(alloc.click, alloc.filled, alloc.notional));
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
                    out.push(answer(click, Qty::ZERO, 0.0));
                    self.dirty = true;
                }
            }
        }
    }

    /// The clock: answer clicks that have waited past `max_wait`.
    pub fn sweep(&mut self, now: NanoTime, out: &mut Burst<Click>) {
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
                out.push(answer(click, Qty::ZERO, 0.0));
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
            out.push(answer(click, Qty::ZERO, 0.0));
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
        let before = self.exposure();
        let after = before + signed(side, qty);
        if after.abs() > self.limits.max_position && after.abs() > before.abs() {
            return refuse("position limit", out, click);
        }
        self.pending[slot(side)].push_back((click, qty, now));
        self.dirty = true;
    }

    /// Where the position would be if everything on order and waiting
    /// filled: the risk node's net, plus what is still to come.
    fn exposure(&self) -> Qty {
        let mut total = self.risk.net;
        for working in self.working.values() {
            for alloc in &working.allocs {
                total = total + signed(working.side, alloc.want - alloc.filled);
            }
        }
        for side in [Side::Bid, Side::Ask] {
            for (_, qty, _) in &self.pending[slot(side)] {
                total = total + signed(side, *qty);
            }
        }
        total
    }

    /// The decision this instant owes the OMS, if what is pending changed:
    /// each side crosses to the far touch for the oldest whole clicks that
    /// fit in one order, or shows nothing.
    ///
    /// One [`Desired`] states both sides at one `as_of`, so a fresh
    /// decision on one side is a fresh one on the other; a side with nothing
    /// waiting states nothing, which is never a cross the OMS would repeat.
    pub fn decide(&mut self, now: NanoTime) -> Option<Desired<EurUsd>> {
        if !std::mem::take(&mut self.dirty) {
            return None;
        }
        let ladder = |side: Side, cap: Option<Px>| -> Ladder {
            let mut total = Qty::ZERO;
            for (_, qty, _) in &self.pending[slot(side)] {
                if total + *qty > self.limits.max_order {
                    break;
                }
                total = total + *qty;
            }
            match cap {
                Some(cap) if total > Qty::ZERO => Ladder::one(Level::new(cap, total)),
                _ => Ladder::none(),
            }
        };
        Some(Desired {
            instrument: EurUsd,
            // A buy crosses up to the ask, a sell down to the bid.
            bids: ladder(Side::Bid, self.touch.ask),
            asks: ladder(Side::Ask, self.touch.bid),
            as_of: now,
            intent: Intent::Cross,
            reduce_only: false,
        })
    }
}

/// A click's answer: what it got, at its average price in the wire's
/// basis points.
fn answer(mut click: Click, filled: Qty, notional: f64) -> Click {
    let qty = filled.to_f64();
    click.payload.filled_qty = qty.round() as u64;
    click.payload.fill_price_bps = if qty > 0.0 {
        (notional / qty * 10_000.0).round() as i64
    } else {
        0
    };
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
        assert_eq!(desired.intent, Intent::Cross);
        assert_eq!(
            desired.bids.best(),
            Some(Level::new(px("1.0852"), qty("5")))
        );
        assert!(desired.asks.is_empty());
        assert_eq!(desk.decide(t(1)), None, "nothing new, nothing owed");
    }

    #[test]
    fn fills_are_allocated_first_in_first_out_and_answered_when_the_order_ends() {
        let mut desk = desk();
        let mut out = Burst::new();
        desk.click(t(1), click(1, SIDE_BUY, 2), &mut out);
        desk.click(t(1), click(2, SIDE_BUY, 3), &mut out);
        desk.decide(t(1));
        desk.sent(&[Request::Place(Order::limit(
            id(1),
            EurUsd,
            Side::Bid,
            qty("5"),
            px("1.0852"),
        ))]);
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
