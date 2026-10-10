//! The shape FIX imposes on order entry, as values.
//!
//! Not a codec — nothing here reads or writes tag=value — but the three
//! things about FIX-style order entry that this crate's edge does not share:
//!
//! - **Every request carries its own id.** A replace (`35=G`) or a cancel
//!   (`35=F`) is a new message with a new [`ClOrdId`], naming the order it
//!   acts on by the previous one (`OrigClOrdID`). An order's id changes every
//!   time it is replaced. This edge's [`Amend`](crate::adapters::execution::edge::Amend) keeps
//!   one [`ClientOrderId`] for the life of the
//!   order.
//! - **An execution report names the message it answers**, by that
//!   message's `ClOrdID`, and the one it replaced, where it replaced one.
//! - **A venue may not have a mass cancel** or a post-only order at all.
//!
//! [`Message`] and [`ExecReport`] are that vocabulary. The test venue,
//! `testing::FixVenue` (feature `execution-testing`), speaks it, and [`ReplaceChain`]
//! is the adapter that puts the OMS in front of anything that does.

use std::collections::HashMap;

use crate::NanoTime;
use crate::adapters::market::{Px, Qty, Side};

use crate::adapters::execution::edge::{Ack, Reject, RejectReason, Report, Request, Retired};
use crate::adapters::execution::exec_id::{ExecId, VenueId};
use crate::adapters::execution::order::{
    ClientOrderId, Epoch, Fill, Instrument, Liquidity, OrderKind, TimeInForce,
};

/// A FIX `ClOrdID`: the id of one *message*, unique for the session.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct ClOrdId(pub u64);

/// A new order, as a venue without an amend-in-place takes it
/// (NewOrderSingle, `35=D`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct NewOrder<I> {
    /// The new order's id.
    pub cl_ord_id: ClOrdId,
    /// The contract.
    pub instrument: I,
    /// Buy or sell.
    pub side: Side,
    /// Quantity.
    pub qty: Qty,
    /// Limit price; `None` for a market order.
    pub price: Option<Px>,
    /// Limit, post-only (where the venue has it) or market.
    pub kind: OrderKind,
    /// Lifetime.
    pub tif: TimeInForce,
}

/// One order-entry message.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Message<I> {
    /// NewOrderSingle (`35=D`).
    New(NewOrder<I>),
    /// OrderCancelReplaceRequest (`35=G`): the order `orig` names, moved to
    /// `price` and `qty`, from now on called `cl_ord_id`.
    ///
    /// Carries the order's contract, side, kind and lifetime as well as its
    /// ids: a replace on the wire restates them (`Symbol`, `Side`, `OrdType`
    /// and `TimeInForce` are required on a `35=G`), and a codec that had to
    /// look them up would be keeping its own copy of every live order.
    Replace {
        /// This message's id, and the order's from now on.
        cl_ord_id: ClOrdId,
        /// The order's current id.
        orig: ClOrdId,
        /// The contract, as the order was sent.
        instrument: I,
        /// The side, as the order was sent.
        side: Side,
        /// The kind, as the order was sent.
        kind: OrderKind,
        /// The lifetime, as the order was sent.
        tif: TimeInForce,
        /// The new price.
        price: Px,
        /// The order's new total — FIX `OrderQty` (tag 38), what has
        /// already filled included — not what is left to show. The venue
        /// rests this less the order's `CumQty`.
        qty: Qty,
    },
    /// OrderCancelRequest (`35=F`), which restates the order's contract and
    /// side beside the id it cancels — both required on the wire.
    Cancel {
        /// This message's id.
        cl_ord_id: ClOrdId,
        /// The order's current id.
        orig: ClOrdId,
        /// The contract, as the order was sent.
        instrument: I,
        /// The side, as the order was sent.
        side: Side,
    },
    /// OrderMassCancelRequest (`35=q`), where the venue has one.
    MassCancel {
        /// This message's id.
        cl_ord_id: ClOrdId,
    },
}

impl<I> Message<I> {
    /// This message's own id.
    pub const fn cl_ord_id(&self) -> ClOrdId {
        match self {
            Message::New(order) => order.cl_ord_id,
            Message::Replace { cl_ord_id, .. }
            | Message::Cancel { cl_ord_id, .. }
            | Message::MassCancel { cl_ord_id } => *cl_ord_id,
        }
    }
}

/// What happened, in an [`ExecReport`] (`ExecType`, `150`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ExecKind<I> {
    /// A new order is working (`150=0`).
    New,
    /// A replace took effect (`150=5`); the order is now `cl_ord_id`.
    Replaced {
        /// The price now resting.
        price: Px,
        /// The order's total — FIX `OrderQty` (tag 38), as the replace
        /// stated it — not what is left: what rests is this less what has
        /// filled.
        qty: Qty,
    },
    /// The order is off the book (`150=4`): a cancel, or an
    /// immediate-or-cancel's unfilled rest.
    Canceled {
        /// What was still showing.
        remaining: Qty,
    },
    /// The order's lifetime ran out (`150=C`): a day order at the close.
    Expired {
        /// What was still showing.
        remaining: Qty,
    },
    /// A message was refused (`150=8`, or a cancel reject, `35=9`).
    Rejected(RejectReason),
    /// An execution (`150=F`).
    Trade(Trade<I>),
}

/// One execution.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Trade<I> {
    /// The venue's id for the execution.
    pub exec_id: ExecId,
    /// The contract.
    pub instrument: I,
    /// The order's side.
    pub side: Side,
    /// Filled in this execution (`LastQty`, tag 32).
    pub qty: Qty,
    /// Filled on the order so far, this execution included (`CumQty`, tag
    /// 14). Carried across a replace: a replace fills nothing.
    pub filled: Qty,
    /// Left open after it (`LeavesQty`, tag 151).
    pub remaining: Qty,
    /// At.
    pub price: Px,
    /// Made or took liquidity.
    pub liquidity: Liquidity,
}

/// An ExecutionReport (`35=8`), or a cancel reject (`35=9`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ExecReport<I> {
    /// The message this answers, or the order's current id for an
    /// unsolicited report (a fill, an expiry).
    pub cl_ord_id: ClOrdId,
    /// The id the order had before, on a replace or a cancel.
    pub orig: Option<ClOrdId>,
    /// What happened.
    pub kind: ExecKind<I>,
    /// The venue's time.
    pub venue_time: NanoTime,
}

/// The adapter between this crate's edge and a FIX-shaped venue: requests out as
/// [`Message`]s, execution reports back as [`Report`]s against the OMS's own
/// ids.
///
/// # The replace chain
///
/// The OMS names an order by one [`ClientOrderId`] for its whole life; the
/// venue renames it on every replace. So the chain keeps, per live order,
/// its **current** venue id — the one the venue last confirmed — and every
/// id minted for it that the venue may still answer on: the order's own, a
/// replace in flight, a cancel in flight. A report on any of them is the
/// OMS's order; a confirmed replace moves the current id; a terminal report
/// forgets them all. The OMS never learns a venue id exists, which is the
/// point: nothing in the diff changes.
///
/// # A replace states the total
///
/// An [`Amend`](crate::adapters::execution::edge::Amend) says what should be left *showing*; a
/// replace's `OrderQty` is the order's *total*, and the venue rests it less
/// what has filled. Sent through unchanged, an amend back to 10 after 3 had
/// filled would rest 7 while the OMS believed 10, and one at or below the
/// filled amount would be refused on every tick. So the chain keeps what
/// has filled on each live order — the venue's `CumQty`, as its last
/// execution stated it ([`Trade::filled`]) — and states a replace as that
/// plus the amend's size. The venue's number rather than a count of the
/// executions the chain saw, so one it missed does not leave every later
/// replace short. A confirmed replace leaves it alone — FIX carries
/// `CumQty` across one — and a terminal report forgets it with the order.
///
/// # Cancel-all without a mass cancel
///
/// Where the venue has none, [`CancelAll`](Request::CancelAll) goes out as
/// one cancel per live order, in id order — skipping an order whose own
/// cancel is already in flight, which the venue would only refuse twice.
/// The OMS moved every working slot to pending-cancel when it sent the
/// request, and each per-order cancel is answered on that order's id, so
/// the slots retire exactly as a mass cancel's reports would retire them
///. Where the venue has one, a
/// refusal of it names no order the chain knows, and crosses as the
/// unattributed reject the OMS reads as a refused cancel-all while one is
/// in flight — and counts, otherwise, as a reject about nothing it sent.
///
/// # What it cannot know
///
/// A FIX execution report carries no commission here, so a [`Fill`]'s fee is
/// zero; a venue that states one is its integration's to fill in.
#[derive(Clone, Debug)]
pub struct ReplaceChain<I> {
    mass_cancel: bool,
    next: u64,
    /// The OMS's order → the venue id it is resting under.
    current: HashMap<ClientOrderId, ClOrdId>,
    /// The OMS's order → the order as it was sent, for what a replace and a
    /// cancel restate on the wire.
    sent: HashMap<ClientOrderId, NewOrder<I>>,
    /// Every venue id the venue may still answer on → the OMS's order.
    owner: HashMap<ClOrdId, ClientOrderId>,
    /// The OMS's order → every venue id minted for it, so a terminal report
    /// forgets them in the length of its own chain rather than a walk of
    /// `owner`.
    minted: HashMap<ClientOrderId, Vec<ClOrdId>>,
    /// The OMS's order → how much of it has filled, as the venue's last
    /// execution on it stated (FIX `CumQty`), for the live orders that have
    /// had one.
    filled: HashMap<ClientOrderId, Qty>,
    /// The OMS's order → the cancel in flight on it, so a cancel-all
    /// rendered per order does not cancel it a second time.
    cancelling: HashMap<ClientOrderId, ClOrdId>,
    /// The id the last mass cancel went out under: the one message that
    /// names no order, and so the one report that may come back naming
    /// none. Only on a venue with a mass cancel.
    pulling: Option<ClOrdId>,
}

impl<I: Instrument> ReplaceChain<I> {
    /// A chain for a venue that has a mass cancel or not, minting under
    /// `epoch` — the process's, as the OMS's ids carry it.
    ///
    /// A `ClOrdId` is unique for the session, and a restarted process is
    /// often still in it: a bare counter would send the last process's ids
    /// again, and the venue refuses every one. So the epoch sits above a
    /// 32-bit counter, as in [`ClientOrderId`]; [`Epoch::ZERO`] is the bare
    /// counter, for a process that persists nothing.
    pub fn new(mass_cancel: bool, epoch: Epoch) -> ReplaceChain<I> {
        ReplaceChain {
            mass_cancel,
            next: (u64::from(epoch.get()) << 32) | 1,
            current: HashMap::new(),
            sent: HashMap::new(),
            owner: HashMap::new(),
            minted: HashMap::new(),
            filled: HashMap::new(),
            cancelling: HashMap::new(),
            pulling: None,
        }
    }

    /// Orders the chain holds a venue id for.
    pub fn live(&self) -> usize {
        self.current.len()
    }

    /// The venue id `order` rests under, if it is live.
    pub fn current(&self, order: ClientOrderId) -> Option<ClOrdId> {
        self.current.get(&order).copied()
    }

    /// Render `requests` as the venue's messages.
    ///
    /// A request for an order the chain holds no id for — already gone at
    /// the venue — sends nothing and is answered at once with an
    /// [`UnknownOrder`](RejectReason::UnknownOrder) reject, the second half
    /// of the pair, so the OMS's slot does not wait on a message never sent.
    pub fn send(
        &mut self,
        now: NanoTime,
        requests: &[Request<I>],
    ) -> (Vec<Message<I>>, Vec<Report<I>>) {
        let mut messages = Vec::new();
        let mut refused = Vec::new();
        for request in requests {
            match *request {
                // `StopPx` is not rendered here: a triggered order sent as
                // the plain one it names would trade on arrival. Refused at
                // once, like an order the chain cannot address.
                Request::Place(order) if order.trigger.is_some() => {
                    refused.push(Report::Reject(Reject {
                        order: Some(order.id),
                        reason: RejectReason::Other,
                        venue_time: None,
                        recv_time: now,
                    }));
                }
                Request::Place(order) => {
                    let id = self.mint(order.id);
                    self.current.insert(order.id, id);
                    let new = NewOrder {
                        cl_ord_id: id,
                        instrument: order.instrument,
                        side: order.side,
                        qty: order.qty,
                        price: order.price,
                        kind: order.kind,
                        tif: order.tif,
                    };
                    self.sent.insert(order.id, new);
                    messages.push(Message::New(new));
                }
                Request::Amend(amend) => match self.resting(amend.order) {
                    Some((orig, sent)) => messages.push(Message::Replace {
                        cl_ord_id: self.mint(amend.order),
                        orig,
                        instrument: sent.instrument,
                        side: sent.side,
                        kind: sent.kind,
                        tif: sent.tif,
                        price: amend.price,
                        // What shows, stated as the total: see "A replace
                        // states the total".
                        qty: self.filled(amend.order) + amend.qty,
                    }),
                    None => refused.push(unknown(now, amend.order)),
                },
                Request::Cancel(order) => match self.resting(order) {
                    Some((orig, sent)) => messages.push(self.cancel(order, orig, sent)),
                    None => refused.push(unknown(now, order)),
                },
                Request::CancelAll if self.mass_cancel => {
                    let id = ClOrdId(self.next);
                    self.next += 1;
                    self.pulling = Some(id);
                    messages.push(Message::MassCancel { cl_ord_id: id });
                }
                Request::CancelAll => {
                    let mut live: Vec<(ClOrdId, ClientOrderId)> = self
                        .current
                        .iter()
                        .filter(|(order, _)| !self.cancelling.contains_key(order))
                        .map(|(order, id)| (*id, *order))
                        .collect();
                    live.sort_unstable();
                    for (orig, order) in live {
                        let sent = self.sent[&order];
                        messages.push(self.cancel(order, orig, sent));
                    }
                }
            }
        }
        (messages, refused)
    }

    /// The id `order` rests under, and the order as it was sent, if it is
    /// live.
    fn resting(&self, order: ClientOrderId) -> Option<(ClOrdId, NewOrder<I>)> {
        Some((self.current(order)?, *self.sent.get(&order)?))
    }

    /// A cancel of `order`, resting under `orig`, noted as in flight.
    fn cancel(&mut self, order: ClientOrderId, orig: ClOrdId, sent: NewOrder<I>) -> Message<I> {
        let cl_ord_id = self.mint(order);
        self.cancelling.insert(order, cl_ord_id);
        Message::Cancel {
            cl_ord_id,
            orig,
            instrument: sent.instrument,
            side: sent.side,
        }
    }

    /// Translate the venue's `reports`, received at `now`, into the OMS's.
    pub fn receive(&mut self, now: NanoTime, reports: &[ExecReport<I>]) -> Vec<Report<I>> {
        let mut out = Vec::new();
        for report in reports {
            let order = self
                .owner
                .get(&report.cl_ord_id)
                .or_else(|| report.orig.and_then(|orig| self.owner.get(&orig)))
                .copied();
            let Some(order) = order else {
                // Nothing the chain holds for a live order. A refused mass
                // cancel names no order, and crosses as exactly that — the
                // OMS reads an unattributed reject as the cancel-all's. So
                // only the mass cancel's own id may cross that way: a
                // reject for an id the chain has already forgotten — the
                // venue refusing a cancel of an order that filled first,
                // which is the ordinary race in a cancel-all sent one
                // cancel at a time — answers an order already retired, and
                // sent on as unattributed it would return every other
                // pending cancel to working and send them all again.
                if let ExecKind::Rejected(reason) = report.kind
                    && self
                        .pulling
                        .take_if(|pulling| *pulling == report.cl_ord_id)
                        .is_some()
                {
                    out.push(Report::Reject(Reject {
                        order: None,
                        reason,
                        venue_time: Some(report.venue_time),
                        recv_time: now,
                    }));
                }
                continue;
            };
            let venue_time = Some(report.venue_time);
            match report.kind {
                ExecKind::New => {
                    // A new order rests as it was sent: the venue restates
                    // it, and the order it was sent as is what it restates.
                    let sent = self.sent.get(&order).copied();
                    let price = sent.and_then(|sent| sent.price);
                    let remaining = sent.map(|sent| sent.qty);
                    out.push(self.ack(order, report, now, price, remaining));
                }
                ExecKind::Replaced { price, qty } => {
                    // The count carries across: a replace fills nothing. What
                    // shows is the total the venue confirmed less it.
                    self.current.insert(order, report.cl_ord_id);
                    let remaining = qty - self.filled(order);
                    out.push(self.ack(order, report, now, Some(price), Some(remaining)));
                }
                ExecKind::Canceled { remaining } => {
                    self.forget(order);
                    out.push(Report::Cancelled(Retired {
                        order,
                        remaining,
                        venue_time,
                        recv_time: now,
                    }));
                }
                ExecKind::Expired { remaining } => {
                    self.forget(order);
                    out.push(Report::Expired(Retired {
                        order,
                        remaining,
                        venue_time,
                        recv_time: now,
                    }));
                }
                ExecKind::Rejected(reason) => {
                    let new_order_refused = report.orig.is_none()
                        && self.current.get(&order) == Some(&report.cl_ord_id);
                    if new_order_refused || reason == RejectReason::UnknownOrder {
                        // The order never lived, or is gone: the slot goes
                        // idle on this, so the chain forgets it too.
                        self.forget(order);
                    } else {
                        // A replace or cancel refused: the order rests under
                        // its current id, and the refused one is dead.
                        self.owner.remove(&report.cl_ord_id);
                        if self.cancelling.get(&order) == Some(&report.cl_ord_id) {
                            self.cancelling.remove(&order);
                        }
                    }
                    out.push(Report::Reject(Reject {
                        order: Some(order),
                        reason,
                        venue_time,
                        recv_time: now,
                    }));
                }
                ExecKind::Trade(trade) => {
                    if trade.remaining <= Qty::ZERO {
                        self.forget(order);
                    } else {
                        self.filled.insert(order, trade.filled);
                    }
                    out.push(Report::Fill(Fill {
                        order,
                        exec_id: trade.exec_id,
                        instrument: trade.instrument,
                        side: trade.side,
                        qty: trade.qty,
                        filled: trade.filled,
                        remaining: trade.remaining,
                        price: trade.price,
                        fee: Qty::ZERO,
                        liquidity: trade.liquidity,
                        venue_time,
                        recv_time: now,
                    }));
                }
            }
        }
        out
    }

    /// A fresh venue id, owned by `order`.
    fn mint(&mut self, order: ClientOrderId) -> ClOrdId {
        let id = ClOrdId(self.next);
        self.next += 1;
        self.owner.insert(id, order);
        self.minted.entry(order).or_default().push(id);
        id
    }

    /// How much of `order` has filled so far.
    fn filled(&self, order: ClientOrderId) -> Qty {
        self.filled.get(&order).copied().unwrap_or(Qty::ZERO)
    }

    fn forget(&mut self, order: ClientOrderId) {
        self.current.remove(&order);
        self.sent.remove(&order);
        self.filled.remove(&order);
        self.cancelling.remove(&order);
        for id in self.minted.remove(&order).unwrap_or_default() {
            self.owner.remove(&id);
        }
    }

    fn ack(
        &self,
        order: ClientOrderId,
        report: &ExecReport<I>,
        now: NanoTime,
        price: Option<Px>,
        remaining: Option<Qty>,
    ) -> Report<I> {
        Report::Ack(Ack {
            order,
            venue_id: VenueId::new(&report.cl_ord_id.0.to_string()).expect("a u64 fits"),
            price,
            remaining,
            venue_time: Some(report.venue_time),
            recv_time: now,
        })
    }
}

/// The answer to a request for an order the venue no longer has.
fn unknown<I>(now: NanoTime, order: ClientOrderId) -> Report<I> {
    Report::Reject(Reject {
        order: Some(order),
        reason: RejectReason::UnknownOrder,
        venue_time: None,
        recv_time: now,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::adapters::execution::edge::Amend;
    use crate::adapters::execution::order::Order;

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

    fn report(cl_ord_id: ClOrdId, orig: Option<ClOrdId>, kind: ExecKind<Es>) -> ExecReport<Es> {
        ExecReport {
            cl_ord_id,
            orig,
            kind,
            venue_time: now(),
        }
    }

    fn trade(last: &str, cum: &str, remaining: &str) -> ExecKind<Es> {
        ExecKind::Trade(Trade {
            exec_id: ExecId::new("t").unwrap(),
            instrument: Es,
            side: Side::Bid,
            qty: qty(last),
            filled: qty(cum),
            remaining: qty(remaining),
            price: px("4999.75"),
            liquidity: Liquidity::Maker,
        })
    }

    fn amend(order: ClientOrderId, size: &str) -> Request<Es> {
        Request::Amend(Amend {
            order,
            instrument: Es,
            price: px("4999.75"),
            qty: qty(size),
            trigger: None,
        })
    }

    /// The size a single amend goes out as.
    fn replace_qty(chain: &mut ReplaceChain<Es>, request: Request<Es>) -> (ClOrdId, Qty) {
        let (messages, refused) = chain.send(now(), &[request]);
        assert!(refused.is_empty(), "{refused:?}");
        let [Message::Replace { cl_ord_id, qty, .. }] = messages.as_slice() else {
            panic!("{messages:?}")
        };
        (*cl_ord_id, *qty)
    }

    /// An amend says what shows; a replace's `OrderQty` is the total. So
    /// after 3 of 10 fill, an amend back to 10 goes out as 13 — and what has
    /// filled survives the rename, follows the venue's `CumQty` over fills,
    /// and goes with the order.
    #[test]
    fn a_replace_states_the_amended_size_plus_what_has_filled() {
        let mut chain = ReplaceChain::new(false, Epoch::ZERO);
        let id = ClientOrderId::new(Epoch::new(1).unwrap(), 1);
        let place = Order::limit(id, Es, Side::Bid, qty("10"), px("4999.75"));
        let (messages, _) = chain.send(now(), &[Request::Place(place)]);
        let new = messages[0].cl_ord_id();
        chain.receive(now(), &[report(new, None, ExecKind::New)]);

        assert_eq!(
            replace_qty(&mut chain, amend(id, "10")).1,
            qty("10"),
            "unfilled"
        );

        chain.receive(now(), &[report(new, None, trade("3", "3", "7"))]);
        let (renamed, total) = replace_qty(&mut chain, amend(id, "10"));
        assert_eq!(total, qty("13"));

        let replaced = ExecKind::Replaced {
            price: px("4999.75"),
            qty: total,
        };
        chain.receive(now(), &[report(renamed, Some(new), replaced)]);
        assert_eq!(
            replace_qty(&mut chain, amend(id, "4")).1,
            qty("7"),
            "a replace fills nothing"
        );

        chain.receive(now(), &[report(renamed, None, trade("2", "5", "8"))]);
        assert_eq!(replace_qty(&mut chain, amend(id, "10")).1, qty("15"));

        chain.receive(now(), &[report(renamed, None, trade("8", "13", "0"))]);
        assert_eq!(chain.live(), 0);
        assert!(chain.filled.is_empty(), "forgotten with the order");
    }

    /// What has filled is the venue's `CumQty`, carried onto the edge's
    /// [`Fill::filled`] and read by the next replace — not a count of the
    /// executions the chain happened to see. After one it missed, a count
    /// would state every later replace short by it.
    #[test]
    fn what_has_filled_is_the_venues_cum_qty_not_a_count() {
        let mut chain = ReplaceChain::new(false, Epoch::ZERO);
        let id = ClientOrderId::new(Epoch::new(1).unwrap(), 1);
        let place = Order::limit(id, Es, Side::Bid, qty("10"), px("4999.75"));
        let (messages, _) = chain.send(now(), &[Request::Place(place)]);
        let new = messages[0].cl_ord_id();

        // The venue's first execution, of 3, never arrived.
        let out = chain.receive(now(), &[report(new, None, trade("2", "5", "5"))]);
        let [Report::Fill(fill)] = out.as_slice() else {
            panic!("{out:?}")
        };
        assert_eq!(
            (fill.qty, fill.filled, fill.remaining),
            (qty("2"), qty("5"), qty("5"))
        );
        assert_eq!(replace_qty(&mut chain, amend(id, "5")).1, qty("10"));
    }

    /// A replace and a cancel restate the order's contract, side, kind and
    /// lifetime as it was sent: what a `35=G` and a `35=F` require beside
    /// the ids, so a codec keeps no copy of its own.
    #[test]
    fn a_replace_and_a_cancel_restate_the_order_as_it_was_sent() {
        let mut chain = ReplaceChain::new(false, Epoch::ZERO);
        let id = ClientOrderId::new(Epoch::new(1).unwrap(), 1);
        let place = Order {
            tif: TimeInForce::Day,
            ..Order::post_only(id, Es, Side::Ask, qty("10"), px("4999.75"))
        };
        let (messages, _) = chain.send(now(), &[Request::Place(place)]);
        let new = messages[0].cl_ord_id();
        chain.receive(now(), &[report(new, None, ExecKind::New)]);

        let (messages, _) = chain.send(now(), &[amend(id, "4")]);
        let [
            Message::Replace {
                orig,
                instrument,
                side,
                kind,
                tif,
                ..
            },
        ] = messages.as_slice()
        else {
            panic!("{messages:?}")
        };
        assert_eq!(
            (*orig, *instrument, *side, *kind, *tif),
            (new, Es, Side::Ask, OrderKind::PostOnly, TimeInForce::Day)
        );

        let (messages, _) = chain.send(now(), &[Request::Cancel(id)]);
        let [
            Message::Cancel {
                orig,
                instrument,
                side,
                ..
            },
        ] = messages.as_slice()
        else {
            panic!("{messages:?}")
        };
        assert_eq!((*orig, *instrument, *side), (new, Es, Side::Ask));
    }

    /// A cancel-all rendered per order skips an order whose own cancel is
    /// still in flight — the venue would refuse a second — and takes it up
    /// again once that cancel was refused and the order is known to rest.
    #[test]
    fn a_cancel_all_does_not_cancel_twice_what_is_already_being_cancelled() {
        let mut chain = ReplaceChain::new(false, Epoch::ZERO);
        let a = ClientOrderId::new(Epoch::new(1).unwrap(), 1);
        let b = ClientOrderId::new(Epoch::new(1).unwrap(), 2);
        let place = |id| Request::Place(Order::limit(id, Es, Side::Bid, qty("1"), px("4999")));
        let (messages, _) = chain.send(now(), &[place(a), place(b)]);
        let (new_a, new_b) = (messages[0].cl_ord_id(), messages[1].cl_ord_id());
        chain.receive(
            now(),
            &[
                report(new_a, None, ExecKind::New),
                report(new_b, None, ExecKind::New),
            ],
        );

        let (messages, _) = chain.send(now(), &[Request::Cancel(a)]);
        let cancel_a = messages[0].cl_ord_id();
        let (messages, refused) = chain.send(now(), &[Request::CancelAll]);
        assert!(refused.is_empty());
        let origs: Vec<ClOrdId> = messages
            .iter()
            .map(|message| match message {
                Message::Cancel { orig, .. } => *orig,
                other => panic!("{other:?}"),
            })
            .collect();
        assert_eq!(origs, [new_b], "a's cancel is already in flight");

        // The venue refused a's cancel: a rests, and the next cancel-all
        // pulls it.
        chain.receive(
            now(),
            &[report(
                cancel_a,
                Some(new_a),
                ExecKind::Rejected(RejectReason::Other),
            )],
        );
        let (messages, _) = chain.send(now(), &[Request::CancelAll]);
        let [Message::Cancel { orig, .. }] = messages.as_slice() else {
            panic!("{messages:?}")
        };
        assert_eq!(
            *orig, new_a,
            "b's cancel from the first cancel-all still stands"
        );
        assert_eq!(chain.live(), 2);
    }

    /// A reject for an order the chain has already forgotten — the venue
    /// refusing the cancel of an order that filled first, the ordinary race
    /// in a cancel-all sent one cancel at a time — is not the cancel-all's
    /// refusal: it names an order already retired, and passing it on
    /// unattributed would return every other pending cancel to working.
    /// Only the mass cancel's own id crosses as an unattributed reject.
    #[test]
    fn a_reject_for_a_forgotten_order_is_not_an_unattributed_reject() {
        let mut chain = ReplaceChain::new(false, Epoch::ZERO);
        let a = ClientOrderId::new(Epoch::new(1).unwrap(), 1);
        let b = ClientOrderId::new(Epoch::new(1).unwrap(), 2);
        let place = |id| Request::Place(Order::limit(id, Es, Side::Bid, qty("1"), px("4999")));
        let (messages, _) = chain.send(now(), &[place(a), place(b)]);
        let (new_a, new_b) = (messages[0].cl_ord_id(), messages[1].cl_ord_id());
        chain.receive(
            now(),
            &[
                report(new_a, None, ExecKind::New),
                report(new_b, None, ExecKind::New),
            ],
        );
        let (messages, _) = chain.send(now(), &[Request::CancelAll]);
        let cancel_a = messages[0].cl_ord_id();

        // a fills before its cancel lands, and the venue then refuses the
        // cancel: nothing for the OMS, which already retired a on the fill.
        let fill = chain.receive(now(), &[report(new_a, None, trade("1", "1", "0"))]);
        assert!(matches!(fill.as_slice(), [Report::Fill(_)]), "{fill:?}");
        let out = chain.receive(
            now(),
            &[report(
                cancel_a,
                Some(new_a),
                ExecKind::Rejected(RejectReason::UnknownOrder),
            )],
        );
        assert!(out.is_empty(), "{out:?}");
        assert_eq!(chain.live(), 1, "b's cancel is still in flight");

        // On a venue with a mass cancel, its own refusal still crosses as
        // the unattributed reject the OMS reads as the cancel-all's.
        let mut chain = ReplaceChain::new(true, Epoch::ZERO);
        let (messages, _) = chain.send(now(), &[place(a)]);
        chain.receive(
            now(),
            &[report(messages[0].cl_ord_id(), None, ExecKind::New)],
        );
        let (messages, _) = chain.send(now(), &[Request::CancelAll]);
        let mass = messages[0].cl_ord_id();
        let out = chain.receive(
            now(),
            &[report(mass, None, ExecKind::Rejected(RejectReason::Other))],
        );
        assert!(
            matches!(out.as_slice(), [Report::Reject(reject)] if reject.order.is_none()),
            "{out:?}"
        );
        // Answered once: a second one is a reject of something else.
        let again = chain.receive(
            now(),
            &[report(mass, None, ExecKind::Rejected(RejectReason::Other))],
        );
        assert!(again.is_empty(), "{again:?}");
        let _ = new_b;
    }

    /// A triggered order is refused, not sent as the plain order it names:
    /// `StopPx` is not rendered, and that order would trade on arrival.
    #[test]
    fn a_triggered_place_is_refused_not_sent_plain() {
        let mut chain = ReplaceChain::new(false, Epoch::ZERO);
        let place = Order {
            trigger: Some(crate::adapters::execution::order::Trigger::stop(
                crate::adapters::execution::order::Reference::Last,
                px("4990"),
            )),
            ..Order::limit(ClientOrderId(1), Es, Side::Ask, qty("1"), px("4989"))
        };
        let (messages, refused) = chain.send(now(), &[Request::Place(place)]);
        assert!(messages.is_empty());
        assert!(
            matches!(refused.as_slice(), [Report::Reject(reject)] if reject.order == Some(ClientOrderId(1))),
            "{refused:?}"
        );
        assert_eq!(chain.live(), 0);
    }
}
