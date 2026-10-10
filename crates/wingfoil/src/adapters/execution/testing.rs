//! A FIX-shaped test venue: the checking harness for the OMS against a venue
//! that is not the one it was first built against.
//!
//! There is no traditional venue to test against, so this is one, small and
//! exact: it takes [`fix::Message`](crate::adapters::execution::fix::Message)s and answers with
//! [`ExecReport`]s, matches against a touch the test sets, and has the
//! capabilities a [`Profile`] gives it — no post-only and no mass cancel, as
//! many exchanges do not. A replace always renames the order
//! (`OrigClOrdID` → `ClOrdID`), which is the one thing about FIX-style entry
//! every venue shares, and states the order's total (`OrderQty`), which the
//! venue rests less what has already filled — FIX's reading, not this
//! edge's, so a chain that confuses the two is caught here.
//!
//! # How it matches
//!
//! Against a touch, not a book: [`FixVenue::touch`] states the best bid and
//! ask on an instrument, and the depth behind them is unlimited. An order
//! that crosses on arrival fills whole at the touch, as a taker; a resting
//! order the touch comes through fills whole at its own price, as a maker.
//! [`FixVenue::execute`] prints part of one, so a partial fill can be
//! scripted. That is enough to exercise every path the OMS has — ack,
//! reject, fill, replace, cancel, expiry — and nothing here pretends to
//! model a queue.
//!
//! Pure, like the OMS: no clock but the instant each call is handed, so a
//! test is a replay.
//!
//! Behind the `testing` feature, off by default: it is a harness, not a
//! venue, and not part of the API a user builds against. The crate's own
//! tests and examples turn it on through a dev-dependency on itself.

use std::collections::{BTreeMap, HashMap, HashSet};

use crate::NanoTime;
use crate::adapters::market::{Px, Qty, Side};

use crate::adapters::execution::edge::RejectReason;
use crate::adapters::execution::edge::TradingState;
use crate::adapters::execution::exec_id::ExecId;
use crate::adapters::execution::fix::{ClOrdId, ExecKind, ExecReport, Message, NewOrder, Trade};
use crate::adapters::execution::order::{Instrument, Liquidity, OrderKind, TimeInForce};

/// What the venue supports.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Profile {
    /// Whether it takes a post-only order. Without it, one is refused as an
    /// order type it does not have.
    pub post_only: bool,
    /// Whether it takes a mass cancel. Without it, one is refused.
    pub mass_cancel: bool,
}

impl Profile {
    /// The common exchange case: neither.
    pub const EXCHANGE: Profile = Profile {
        post_only: false,
        mass_cancel: false,
    };
}

/// One order resting at the venue, under its current id.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Working<I> {
    /// The contract.
    pub instrument: I,
    /// Buy or sell.
    pub side: Side,
    /// Resting at.
    pub price: Px,
    /// Still showing.
    pub remaining: Qty,
    /// Filled so far (FIX `CumQty`, tag 14) — what a replace's `OrderQty`
    /// is read against.
    pub filled: Qty,
    /// As it was sent.
    pub kind: OrderKind,
    /// As it was sent.
    pub tif: TimeInForce,
}

/// The venue.
#[derive(Clone, Debug)]
pub struct FixVenue<I> {
    profile: Profile,
    /// Resting orders by current id — ordered, so every walk is stated.
    working: BTreeMap<ClOrdId, Working<I>>,
    /// Every id the session has seen: a `ClOrdID` is never reused.
    seen: HashSet<ClOrdId>,
    /// Best bid and ask per instrument.
    touch: HashMap<I, (Option<Px>, Option<Px>)>,
    state: TradingState,
    executions: u64,
}

impl<I: Instrument> FixVenue<I> {
    /// An empty venue with `profile`'s capabilities.
    pub fn new(profile: Profile) -> FixVenue<I> {
        FixVenue {
            profile,
            working: BTreeMap::new(),
            seen: HashSet::new(),
            touch: HashMap::new(),
            state: TradingState::Open,
            executions: 0,
        }
    }

    /// What it supports.
    pub const fn profile(&self) -> Profile {
        self.profile
    }

    /// The order resting under `id`, if one is.
    pub fn working(&self, id: ClOrdId) -> Option<Working<I>> {
        self.working.get(&id).copied()
    }

    /// Every resting order, by id.
    pub fn resting(&self) -> impl Iterator<Item = (ClOrdId, Working<I>)> + '_ {
        self.working.iter().map(|(id, order)| (*id, *order))
    }

    /// Move the venue's trading state. Closing expires every day order;
    /// while it is not open, a new order or a replace is refused and a
    /// cancel is taken.
    pub fn set_state(&mut self, now: NanoTime, state: TradingState) -> Vec<ExecReport<I>> {
        self.state = state;
        if state != TradingState::Closed {
            return Vec::new();
        }
        let expiring: Vec<ClOrdId> = self
            .working
            .iter()
            .filter(|(_, order)| order.tif == TimeInForce::Day)
            .map(|(id, _)| *id)
            .collect();
        expiring
            .into_iter()
            .map(|id| {
                let order = self.working.remove(&id).expect("listed");
                ExecReport {
                    cl_ord_id: id,
                    orig: None,
                    kind: ExecKind::Expired {
                        remaining: order.remaining,
                    },
                    venue_time: now,
                }
            })
            .collect()
    }

    /// The trading state.
    pub const fn state(&self) -> TradingState {
        self.state
    }

    /// Take `messages`, in order, and answer each.
    pub fn send(&mut self, now: NanoTime, messages: &[Message<I>]) -> Vec<ExecReport<I>> {
        let mut out = Vec::new();
        for message in messages {
            let id = message.cl_ord_id();
            if !self.seen.insert(id) {
                // A reused id is refused whatever it asks for.
                out.push(reject(now, id, None, RejectReason::Other));
                continue;
            }
            let adds = matches!(message, Message::New(_) | Message::Replace { .. });
            if adds && self.state != TradingState::Open {
                let orig = match *message {
                    Message::Replace { orig, .. } => Some(orig),
                    _ => None,
                };
                out.push(reject(now, id, orig, RejectReason::Other));
                continue;
            }
            match *message {
                Message::New(order) => self.new_order(now, order, &mut out),
                Message::Replace {
                    cl_ord_id,
                    orig,
                    price,
                    qty,
                    ..
                } => self.replace(now, cl_ord_id, orig, price, qty, &mut out),
                Message::Cancel {
                    cl_ord_id, orig, ..
                } => match self.working.remove(&orig) {
                    Some(order) => out.push(ExecReport {
                        cl_ord_id,
                        orig: Some(orig),
                        kind: ExecKind::Canceled {
                            remaining: order.remaining,
                        },
                        venue_time: now,
                    }),
                    None => out.push(reject(
                        now,
                        cl_ord_id,
                        Some(orig),
                        RejectReason::UnknownOrder,
                    )),
                },
                Message::MassCancel { cl_ord_id } => {
                    if !self.profile.mass_cancel {
                        out.push(reject(now, cl_ord_id, None, RejectReason::Other));
                        continue;
                    }
                    for (id, order) in std::mem::take(&mut self.working) {
                        out.push(ExecReport {
                            cl_ord_id: id,
                            orig: None,
                            kind: ExecKind::Canceled {
                                remaining: order.remaining,
                            },
                            venue_time: now,
                        });
                    }
                }
            }
        }
        out
    }

    /// State the touch on `instrument`, and fill what it comes through.
    pub fn touch(
        &mut self,
        now: NanoTime,
        instrument: I,
        bid: Option<Px>,
        ask: Option<Px>,
    ) -> Vec<ExecReport<I>> {
        self.touch.insert(instrument, (bid, ask));
        let crossed: Vec<ClOrdId> = self
            .working
            .iter()
            .filter(|(_, order)| {
                order.instrument == instrument
                    && self.crosses(instrument, order.side, Some(order.price))
            })
            .map(|(id, _)| *id)
            .collect();
        let mut out = Vec::new();
        for id in crossed {
            let order = self.working.remove(&id).expect("listed");
            out.push(self.trade(
                now,
                id,
                order,
                order.remaining,
                order.price,
                Liquidity::Maker,
            ));
        }
        out
    }

    /// A print of `qty` against the resting order `id`, as a maker at its
    /// own price.
    ///
    /// It exists so a partial fill can be scripted: the touch fills whole,
    /// and a replace after a partial is where FIX's `OrderQty` and this
    /// edge's shown size part. `None`, and nothing done, where nothing rests
    /// under `id`, or `qty` is not above zero, or is more than it shows.
    pub fn execute(&mut self, now: NanoTime, id: ClOrdId, qty: Qty) -> Option<ExecReport<I>> {
        let order = self.working.get(&id).copied()?;
        if qty <= Qty::ZERO || qty > order.remaining {
            return None;
        }
        let report = self.trade(now, id, order, qty, order.price, Liquidity::Maker);
        if qty == order.remaining {
            self.working.remove(&id);
        } else {
            self.working.insert(
                id,
                Working {
                    remaining: order.remaining - qty,
                    filled: order.filled + qty,
                    ..order
                },
            );
        }
        Some(report)
    }

    fn new_order(&mut self, now: NanoTime, order: NewOrder<I>, out: &mut Vec<ExecReport<I>>) {
        let id = order.cl_ord_id;
        let refused = if order.qty <= Qty::ZERO {
            Some(RejectReason::Other)
        } else if order.kind == OrderKind::PostOnly && !self.profile.post_only {
            // An order type this venue does not have.
            Some(RejectReason::Other)
        } else if order.kind != OrderKind::Market && order.price.is_none() {
            Some(RejectReason::Other)
        } else if order.kind == OrderKind::PostOnly
            && self.crosses(order.instrument, order.side, order.price)
        {
            Some(RejectReason::PostOnlyWouldCross)
        } else if order.kind == OrderKind::Market
            && self.far(order.instrument, order.side).is_none()
        {
            Some(RejectReason::Other)
        } else {
            None
        };
        if let Some(reason) = refused {
            out.push(reject(now, id, None, reason));
            return;
        }
        out.push(ExecReport {
            cl_ord_id: id,
            orig: None,
            kind: ExecKind::New,
            venue_time: now,
        });
        let working = Working {
            instrument: order.instrument,
            side: order.side,
            price: order.price.unwrap_or_default(),
            remaining: order.qty,
            filled: Qty::ZERO,
            kind: order.kind,
            tif: order.tif,
        };
        let price = if order.kind == OrderKind::Market {
            None
        } else {
            order.price
        };
        if self.crosses(order.instrument, order.side, price) {
            let at = self.far(order.instrument, order.side).expect("it crosses");
            out.push(self.trade(now, id, working, order.qty, at, Liquidity::Taker));
        } else if matches!(
            order.tif,
            TimeInForce::ImmediateOrCancel | TimeInForce::FillOrKill
        ) {
            out.push(ExecReport {
                cl_ord_id: id,
                orig: None,
                kind: ExecKind::Canceled {
                    remaining: order.qty,
                },
                venue_time: now,
            });
        } else {
            self.working.insert(id, working);
        }
    }

    fn replace(
        &mut self,
        now: NanoTime,
        id: ClOrdId,
        orig: ClOrdId,
        price: Px,
        qty: Qty,
        out: &mut Vec<ExecReport<I>>,
    ) {
        let Some(order) = self.working.get(&orig).copied() else {
            out.push(reject(now, id, Some(orig), RejectReason::UnknownOrder));
            return;
        };
        // `qty` is `OrderQty`, the order's total: at or below what has
        // filled it leaves nothing to show.
        if qty <= order.filled {
            out.push(reject(now, id, Some(orig), RejectReason::Other));
            return;
        }
        let crosses = self.crosses(order.instrument, order.side, Some(price));
        if order.kind == OrderKind::PostOnly && crosses {
            out.push(reject(
                now,
                id,
                Some(orig),
                RejectReason::PostOnlyWouldCross,
            ));
            return;
        }
        self.working.remove(&orig);
        let moved = Working {
            price,
            remaining: qty - order.filled,
            ..order
        };
        out.push(ExecReport {
            cl_ord_id: id,
            orig: Some(orig),
            kind: ExecKind::Replaced { price, qty },
            venue_time: now,
        });
        if crosses {
            let at = self.far(order.instrument, order.side).expect("it crosses");
            out.push(self.trade(now, id, moved, moved.remaining, at, Liquidity::Taker));
        } else {
            self.working.insert(id, moved);
        }
    }

    /// `qty` of `order` executed at `price`; `order` is as it stood before.
    fn trade(
        &mut self,
        now: NanoTime,
        id: ClOrdId,
        order: Working<I>,
        qty: Qty,
        price: Px,
        liquidity: Liquidity,
    ) -> ExecReport<I> {
        self.executions += 1;
        ExecReport {
            cl_ord_id: id,
            orig: None,
            kind: ExecKind::Trade(Trade {
                exec_id: ExecId::new(&format!("T{}", self.executions)).expect("short"),
                instrument: order.instrument,
                side: order.side,
                qty,
                filled: order.filled + qty,
                remaining: order.remaining - qty,
                price,
                liquidity,
            }),
            venue_time: now,
        }
    }

    /// The touch on the other side of `side`: what a buy would lift.
    fn far(&self, instrument: I, side: Side) -> Option<Px> {
        let (bid, ask) = self.touch.get(&instrument).copied().unwrap_or_default();
        match side {
            Side::Bid => ask,
            Side::Ask => bid,
        }
    }

    /// Whether an order at `price` (`None` is a market order) would trade on
    /// arrival.
    fn crosses(&self, instrument: I, side: Side, price: Option<Px>) -> bool {
        match (self.far(instrument, side), price) {
            (None, _) => false,
            (Some(_), None) => true,
            (Some(far), Some(price)) => match side {
                Side::Bid => price >= far,
                Side::Ask => price <= far,
            },
        }
    }
}

fn reject<I>(
    now: NanoTime,
    id: ClOrdId,
    orig: Option<ClOrdId>,
    reason: RejectReason,
) -> ExecReport<I> {
    ExecReport {
        cl_ord_id: id,
        orig,
        kind: ExecKind::Rejected(reason),
        venue_time: now,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash, PartialOrd, Ord)]
    struct Es;

    fn px(s: &str) -> Px {
        Px::parse(s).unwrap()
    }

    fn qty(s: &str) -> Qty {
        Qty::parse(s).unwrap()
    }

    fn now() -> NanoTime {
        NanoTime::from(1_000u64)
    }

    fn limit(id: u64, side: Side, price: &str, kind: OrderKind) -> Message<Es> {
        Message::New(NewOrder {
            cl_ord_id: ClOrdId(id),
            instrument: Es,
            side,
            qty: qty("2"),
            price: Some(px(price)),
            kind,
            tif: TimeInForce::GoodTillCancel,
        })
    }

    fn venue(profile: Profile) -> FixVenue<Es> {
        let mut venue = FixVenue::new(profile);
        venue.touch(now(), Es, Some(px("5000")), Some(px("5000.25")));
        venue
    }

    fn kinds(reports: &[ExecReport<Es>]) -> Vec<ExecKind<Es>> {
        reports.iter().map(|r| r.kind).collect()
    }

    #[test]
    fn a_passive_limit_rests_and_the_touch_fills_it_as_a_maker() {
        let mut venue = venue(Profile::EXCHANGE);
        let out = venue.send(now(), &[limit(1, Side::Bid, "4999.75", OrderKind::Limit)]);
        assert_eq!(kinds(&out), [ExecKind::New]);
        assert!(venue.working(ClOrdId(1)).is_some());

        let out = venue.touch(now(), Es, Some(px("4999.50")), Some(px("4999.75")));
        let [
            ExecReport {
                cl_ord_id,
                kind: ExecKind::Trade(trade),
                ..
            },
        ] = out.as_slice()
        else {
            panic!("{out:?}")
        };
        assert_eq!(*cl_ord_id, ClOrdId(1));
        assert_eq!(
            (trade.price, trade.liquidity),
            (px("4999.75"), Liquidity::Maker)
        );
        assert!(venue.working(ClOrdId(1)).is_none());
    }

    #[test]
    fn a_crossing_limit_takes_the_touch() {
        let mut venue = venue(Profile::EXCHANGE);
        let out = venue.send(now(), &[limit(1, Side::Bid, "5001", OrderKind::Limit)]);
        assert!(matches!(
            out.as_slice(),
            [
                ExecReport { kind: ExecKind::New, .. },
                ExecReport { kind: ExecKind::Trade(Trade { price, liquidity: Liquidity::Taker, .. }), .. }
            ] if *price == px("5000.25")
        ));
    }

    #[test]
    fn post_only_is_refused_where_the_venue_has_none_and_checked_where_it_does() {
        let mut exchange = venue(Profile::EXCHANGE);
        let out = exchange.send(now(), &[limit(1, Side::Bid, "4999", OrderKind::PostOnly)]);
        assert_eq!(kinds(&out), [ExecKind::Rejected(RejectReason::Other)]);

        let mut with = venue(Profile {
            post_only: true,
            mass_cancel: true,
        });
        let out = with.send(
            now(),
            &[limit(1, Side::Bid, "5000.25", OrderKind::PostOnly)],
        );
        assert_eq!(
            kinds(&out),
            [ExecKind::Rejected(RejectReason::PostOnlyWouldCross)]
        );
        let out = with.send(now(), &[limit(2, Side::Bid, "5000", OrderKind::PostOnly)]);
        assert_eq!(kinds(&out), [ExecKind::New]);
    }

    /// A replace renames the order; the old id is gone, and a reused id is
    /// refused.
    #[test]
    fn a_replace_renames_the_order() {
        let mut venue = venue(Profile::EXCHANGE);
        venue.send(now(), &[limit(1, Side::Ask, "5001", OrderKind::Limit)]);
        let out = venue.send(
            now(),
            &[Message::Replace {
                cl_ord_id: ClOrdId(2),
                orig: ClOrdId(1),
                instrument: Es,
                side: Side::Ask,
                kind: OrderKind::Limit,
                tif: TimeInForce::GoodTillCancel,
                price: px("5000.75"),
                qty: qty("3"),
            }],
        );
        assert_eq!(
            out,
            [ExecReport {
                cl_ord_id: ClOrdId(2),
                orig: Some(ClOrdId(1)),
                kind: ExecKind::Replaced {
                    price: px("5000.75"),
                    qty: qty("3")
                },
                venue_time: now(),
            }]
        );
        assert!(venue.working(ClOrdId(1)).is_none());
        assert_eq!(venue.working(ClOrdId(2)).unwrap().remaining, qty("3"));

        let stale = venue.send(
            now(),
            &[Message::Cancel {
                cl_ord_id: ClOrdId(3),
                orig: ClOrdId(1),
                instrument: Es,
                side: Side::Ask,
            }],
        );
        assert_eq!(
            kinds(&stale),
            [ExecKind::Rejected(RejectReason::UnknownOrder)]
        );
        let reused = venue.send(now(), &[limit(2, Side::Ask, "5002", OrderKind::Limit)]);
        assert_eq!(kinds(&reused), [ExecKind::Rejected(RejectReason::Other)]);
    }

    fn replace(id: u64, orig: u64, size: &str) -> Message<Es> {
        Message::Replace {
            cl_ord_id: ClOrdId(id),
            orig: ClOrdId(orig),
            instrument: Es,
            side: Side::Ask,
            kind: OrderKind::Limit,
            tif: TimeInForce::GoodTillCancel,
            price: px("5000.75"),
            qty: qty(size),
        }
    }

    /// Every execution reports FIX's `CumQty` beside `LeavesQty`: what has
    /// filled on the order so far, carried across a replace that renamed it
    /// and changed what shows, up to the touch filling the rest.
    #[test]
    fn an_execution_reports_what_has_filled_on_the_order_so_far() {
        let cum = |report: &ExecReport<Es>| match report.kind {
            ExecKind::Trade(trade) => (trade.qty, trade.filled, trade.remaining),
            other => panic!("{other:?}"),
        };
        let mut venue = venue(Profile::EXCHANGE);
        venue.send(now(), &[limit(1, Side::Ask, "5001", OrderKind::Limit)]);
        let first = venue.execute(now(), ClOrdId(1), qty("0.5")).unwrap();
        assert_eq!(cum(&first), (qty("0.5"), qty("0.5"), qty("1.5")));

        // A total of 3 rests 2.5; a replace fills nothing.
        venue.send(now(), &[replace(2, 1, "3")]);
        let second = venue.execute(now(), ClOrdId(2), qty("1")).unwrap();
        assert_eq!(cum(&second), (qty("1"), qty("1.5"), qty("1.5")));

        let out = venue.touch(now(), Es, Some(px("5001")), Some(px("5001.25")));
        let [last] = out.as_slice() else {
            panic!("{out:?}")
        };
        assert_eq!(cum(last), (qty("1.5"), qty("3"), Qty::ZERO));
    }

    /// A replace's `qty` is FIX's `OrderQty`, the order's total: after 1 of
    /// 2 fills, a replace to 3 rests 2 and reports 3, and one at or below
    /// the 1 filled is refused and leaves the order where it was. A print
    /// is refused past what is showing.
    #[test]
    fn a_replace_states_the_total_and_rests_it_less_what_has_filled() {
        let mut venue = venue(Profile::EXCHANGE);
        venue.send(now(), &[limit(1, Side::Ask, "5001", OrderKind::Limit)]);
        assert_eq!(venue.execute(now(), ClOrdId(1), qty("3")), None);
        let print = venue.execute(now(), ClOrdId(1), qty("1")).unwrap();
        assert!(matches!(
            print.kind,
            ExecKind::Trade(Trade { qty: q, remaining: r, liquidity: Liquidity::Maker, .. })
                if q == qty("1") && r == qty("1")
        ));
        let rested = venue.working(ClOrdId(1)).unwrap();
        assert_eq!((rested.remaining, rested.filled), (qty("1"), qty("1")));

        for (id, at_or_below) in [(2, "1"), (3, "0.5")] {
            let out = venue.send(now(), &[replace(id, 1, at_or_below)]);
            assert_eq!(kinds(&out), [ExecKind::Rejected(RejectReason::Other)]);
            assert_eq!(venue.working(ClOrdId(1)), Some(rested), "untouched");
        }

        let out = venue.send(now(), &[replace(4, 1, "3")]);
        assert_eq!(
            kinds(&out),
            [ExecKind::Replaced {
                price: px("5000.75"),
                qty: qty("3")
            }]
        );
        let moved = venue.working(ClOrdId(4)).unwrap();
        assert_eq!((moved.remaining, moved.filled), (qty("2"), qty("1")));
    }

    #[test]
    fn a_mass_cancel_is_refused_where_the_venue_has_none() {
        let mut exchange = venue(Profile::EXCHANGE);
        exchange.send(now(), &[limit(1, Side::Bid, "4999", OrderKind::Limit)]);
        let out = exchange.send(
            now(),
            &[Message::MassCancel {
                cl_ord_id: ClOrdId(2),
            }],
        );
        assert_eq!(kinds(&out), [ExecKind::Rejected(RejectReason::Other)]);
        assert!(exchange.working(ClOrdId(1)).is_some());

        let mut with = venue(Profile {
            post_only: true,
            mass_cancel: true,
        });
        with.send(now(), &[limit(1, Side::Bid, "4999", OrderKind::Limit)]);
        let out = with.send(
            now(),
            &[Message::MassCancel {
                cl_ord_id: ClOrdId(2),
            }],
        );
        assert_eq!(
            kinds(&out),
            [ExecKind::Canceled {
                remaining: qty("2")
            }]
        );
        assert_eq!(with.resting().count(), 0);
    }

    #[test]
    fn an_ioc_that_does_not_cross_is_cancelled() {
        let mut venue = venue(Profile::EXCHANGE);
        let out = venue.send(
            now(),
            &[Message::New(NewOrder {
                tif: TimeInForce::ImmediateOrCancel,
                ..match limit(1, Side::Bid, "4999", OrderKind::Limit) {
                    Message::New(order) => order,
                    _ => unreachable!(),
                }
            })],
        );
        assert_eq!(
            kinds(&out),
            [
                ExecKind::New,
                ExecKind::Canceled {
                    remaining: qty("2")
                }
            ]
        );
    }
}
