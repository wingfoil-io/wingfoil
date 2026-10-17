//! The FIX bridge as a graph node: [`FixBridgeOp`], wired through
//! [`FixOps`].
//!
//! [`FixBridge`] has two mutating entry points — [`receive`](FixBridge::receive)
//! and [`send`](FixBridge::send) — and, as with the OMS, the order they
//! are called in within one instant is part of what they mean. It is the
//! node's, stated once:
//!
//! 1. **executions** — [`FixBridge::receive`], the whole burst in order;
//! 2. **requests** — [`FixBridge::send`], the whole burst in order.
//!
//! Executions first, because a replace states the order's total as what has
//! filled plus what should show: a fill the venue has already reported has
//! to be counted before an amend in the same instant is rendered, or the
//! venue rests less than the OMS believes. And an order a terminal report
//! retired in this instant is gone by the time a request names it, so the
//! request is answered at once as unknown rather than sent to be refused.
//!
//! The reports out follow the same order: the venue's, translated, then the
//! refusals [`send`](FixBridge::send) answers at once.
//!
//! # Sending from the node
//!
//! [`FixOps::fix_bridge`] hands the messages on as a stream and leaves
//! sending them to the caller. A transport that can refuse a message — a
//! bounded queue full, a session closed — then has nowhere to say so: the
//! bridge believes the message in flight and the OMS's slot waits on an
//! answer nobody will send. [`FixOps::fix_bridge_sending`] puts the send
//! in the node instead, after `send` renders the instant's messages, and a
//! message the transport refuses is answered at once, in the same instant,
//! as the venue's refusal ([`ExecReport::refusal`]) received through the
//! bridge. No second cycle and no edge back: the answer is the node's own.
//!
//! The executions are the venue's — a session's inbound side, a source —
//! and the requests have already crossed the OMS's feedback cut, so the
//! node sits on the forward side of it and adds none. A venue that answers
//! in the instant it is sent to, as the test venue does, cannot feed this
//! node without a second cycle; it holds the bridge beside itself instead
//! (`testing::SimVenue`) and calls it in the same order.

use std::marker::PhantomData;

use crate::op::{Activation, Ctx, Op, Tick};
use crate::prelude::*;
use anyhow::Result;

use super::{ExecReport, FixBridge, Message};
use crate::NanoTime;
use crate::trading::execution::edge::RejectReason;
use crate::trading::execution::edge::{Report, Request, non_empty};
use crate::trading::execution::order::{Epoch, Instrument};

/// The bridge as an op: a [`FixBridge`] held as node state, fed the
/// venue's executions and then the OMS's requests, in the order the
/// [module docs](self) state, emitting the messages to send and the
/// reports for the OMS.
///
/// Wire it with [`FixOps`]; the op itself is public so a compiled graph can
/// name it.
///
/// - `Cfg` — whether the venue has a mass cancel, and the [`Epoch`] the
///   bridge's ids are minted in.
/// - `State` — the [`FixBridge`], built from the config on the first
///   cycle.
/// - `Out` — `(messages, reports)`, every cycle the node runs; either burst
///   may be empty.
pub struct FixBridgeOp<I>(PhantomData<I>);

#[crate::op(build = fix_bridge_op)]
impl<I> Op for FixBridgeOp<I>
where
    I: Instrument + 'static,
{
    type Cfg = (bool, Epoch);
    type State = Option<FixBridge<I>>;
    type In<'a> = (
        (&'a Burst<ExecReport<I>>, bool),
        (&'a Burst<Request<I>>, bool),
    );
    type Out = (Burst<Message<I>>, Burst<Report<I>>);
    const ACTIVATION: Activation = Activation::NONE;

    fn cycle(
        cfg: &mut (bool, Epoch),
        state: &mut Option<FixBridge<I>>,
        input: Self::In<'_>,
        ctx: &mut Ctx<'_>,
    ) -> Result<Tick<(Burst<Message<I>>, Burst<Report<I>>)>> {
        let (mass_cancel, epoch) = *cfg;
        let bridge = state.get_or_insert_with(|| FixBridge::new(mass_cancel, epoch));
        let (messages, reports) = step(bridge, ctx.time(), input);
        Ok(Tick::Value((messages.into_iter().collect(), reports)))
    }
}

/// One instant of the bridge, in the order the [module docs](self) state —
/// the executions received, then the requests rendered — returning the
/// messages to send and the reports so far: the translations, then the
/// refusals `send` answered at once. Both ops run exactly this, so the
/// order cannot drift between them.
#[allow(clippy::type_complexity)]
fn step<I: Instrument>(
    bridge: &mut FixBridge<I>,
    now: NanoTime,
    input: ((&Burst<ExecReport<I>>, bool), (&Burst<Request<I>>, bool)),
) -> (Vec<Message<I>>, Burst<Report<I>>) {
    let ((executions, executed), (requests, requested)) = input;
    let mut messages = Vec::new();
    let mut reports: Burst<Report<I>> = Burst::new();
    if executed {
        reports.extend(bridge.receive(now, executions));
    }
    if requested {
        let (sent, refused) = bridge.send(now, requests);
        messages = sent;
        reports.extend(refused);
    }
    (messages, reports)
}

/// The bridge as an op that sends what it renders: [`FixBridgeOp`], with
/// each message handed to `send` and a refusal answered through the bridge in
/// the same instant ([module docs](self), *Sending from the node*).
///
/// - `Cfg` — whether the venue has a mass cancel, the [`Epoch`], and
///   `send`.
/// - `State` — the [`FixBridge`].
/// - `Out` — the reports for the OMS, ticking only on a cycle that had any.
pub struct FixBridgeSendOp<I, F>(PhantomData<(I, F)>);

#[crate::op(build = fix_bridge_send_op)]
impl<I, F> Op for FixBridgeSendOp<I, F>
where
    I: Instrument + 'static,
    F: FnMut(&Message<I>, NanoTime) -> std::result::Result<(), RejectReason> + 'static,
{
    type Cfg = (bool, Epoch, F);
    type State = Option<FixBridge<I>>;
    type In<'a> = (
        (&'a Burst<ExecReport<I>>, bool),
        (&'a Burst<Request<I>>, bool),
    );
    type Out = Burst<Report<I>>;
    const ACTIVATION: Activation = Activation::NONE;

    fn cycle(
        cfg: &mut (bool, Epoch, F),
        state: &mut Option<FixBridge<I>>,
        input: Self::In<'_>,
        ctx: &mut Ctx<'_>,
    ) -> Result<Tick<Burst<Report<I>>>> {
        let (mass_cancel, epoch, send) = cfg;
        let bridge = state.get_or_insert_with(|| FixBridge::new(*mass_cancel, *epoch));
        let now = ctx.time();
        let (messages, mut reports) = step(bridge, now, input);
        for message in &messages {
            if let Err(reason) = send(message, now) {
                let refusal = ExecReport::refusal(message, reason, now);
                reports.extend(bridge.receive(now, &[refusal]));
            }
        }
        // Nothing to report is nothing to tick: the stream ticks only on a
        // cycle that had any, with no filter behind it.
        Ok(if reports.is_empty() {
            Tick::Quiet
        } else {
            Tick::Value(reports)
        })
    }
}

/// The FIX bridge, wired on the OMS's request stream. Out of any prelude:
/// `use wingfoil::trading::execution::fix::FixOps;`.
///
/// Two inputs, applied within one instant in this order whichever of them
/// ticked:
///
/// 1. `executions` — the venue's execution reports, translated onto the
///    OMS's ids;
/// 2. `self` — the OMS's requests, already through the OMS's feedback
///    cut, rendered as messages — or refused at once where they name an
///    order the bridge no longer holds.
///
/// Returns the messages for the venue, ticking only on a cycle that sent
/// any, and the reports for the OMS's `reports` input — the translations,
/// then the refusals — ticking only on a cycle that had any.
pub trait FixOps<I: Instrument + 'static> {
    /// The bridge for a venue that has a mass cancel or not, minting under
    /// `epoch` (see [`FixBridge::new`]): see the trait docs for the
    /// inputs and the order they are applied in.
    #[must_use = "a dropped stream stays wired and cycles every tick, producing an unread value"]
    fn fix_bridge(
        &self,
        mass_cancel: bool,
        epoch: Epoch,
        executions: &Stream<Burst<ExecReport<I>>>,
    ) -> (Stream<Burst<Message<I>>>, Stream<Burst<Report<I>>>);

    /// [`fix_bridge`](Self::fix_bridge), sending each message itself:
    /// `send` is handed every message the instant renders, at the engine
    /// time, and one it refuses is answered as the venue's refusal for the
    /// reason it gives, in the same instant (module docs, *Sending from the
    /// node*). Returns the reports, ticking only on a cycle that had any.
    #[must_use = "a dropped stream stays wired and cycles every tick, producing an unread value"]
    fn fix_bridge_sending<F>(
        &self,
        mass_cancel: bool,
        epoch: Epoch,
        executions: &Stream<Burst<ExecReport<I>>>,
        send: F,
    ) -> Stream<Burst<Report<I>>>
    where
        F: FnMut(&Message<I>, NanoTime) -> std::result::Result<(), RejectReason> + 'static;
}

impl<I: Instrument + 'static> FixOps<I> for Stream<Burst<Request<I>>> {
    fn fix_bridge(
        &self,
        mass_cancel: bool,
        epoch: Epoch,
        executions: &Stream<Burst<ExecReport<I>>>,
    ) -> (Stream<Burst<Message<I>>>, Stream<Burst<Report<I>>>) {
        let executions = executions.handle();
        let node = self
            .wire(move |b, requests| b.fix_bridge_op(executions, requests, (mass_cancel, epoch)));
        let messages = non_empty(&node, |(messages, _)| messages);
        let reports = non_empty(&node, |(_, reports)| reports);
        (messages, reports)
    }

    fn fix_bridge_sending<F>(
        &self,
        mass_cancel: bool,
        epoch: Epoch,
        executions: &Stream<Burst<ExecReport<I>>>,
        send: F,
    ) -> Stream<Burst<Report<I>>>
    where
        F: FnMut(&Message<I>, NanoTime) -> std::result::Result<(), RejectReason> + 'static,
    {
        let executions = executions.handle();
        self.wire(move |b, requests| {
            b.fix_bridge_send_op(executions, requests, (mass_cancel, epoch, send))
        })
    }
}

#[cfg(test)]
mod tests {
    use crate::trading::market::{Px, Qty, Side};
    use crate::{NanoTime, RunFor, RunMode};

    use super::*;
    use crate::trading::execution::edge::{Ack, Amend, Reject, RejectReason};
    use crate::trading::execution::exec_id::{ExecId, VenueId};
    use crate::trading::execution::fix::{ClOrdId, ExecKind, NewOrder, Trade};
    use crate::trading::execution::order::{
        ClientOrderId, Fill, Liquidity, Order, OrderKind, TimeInForce,
    };

    #[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash, PartialOrd, Ord)]
    struct Es;

    fn t(nanos: u64) -> NanoTime {
        NanoTime::new(nanos)
    }

    fn px(s: &str) -> Px {
        Px::parse(s).unwrap()
    }

    fn qty(s: &str) -> Qty {
        Qty::parse(s).unwrap()
    }

    fn place(id: u64) -> Request<Es> {
        Request::Place(Order {
            id: ClientOrderId(id),
            instrument: Es,
            side: Side::Bid,
            qty: qty("2"),
            price: Some(px("100")),
            kind: OrderKind::PostOnly,
            tif: TimeInForce::GoodTillCancel,
            ..Order::default()
        })
    }

    fn new(cl_ord_id: u64) -> Message<Es> {
        Message::New(NewOrder {
            cl_ord_id: ClOrdId(cl_ord_id),
            instrument: Es,
            side: Side::Bid,
            qty: qty("2"),
            price: Some(px("100")),
            kind: OrderKind::PostOnly,
            tif: TimeInForce::GoodTillCancel,
        })
    }

    fn report(cl_ord_id: u64, kind: ExecKind<Es>, at: u64) -> (ExecReport<Es>, u64) {
        let report = ExecReport {
            cl_ord_id: ClOrdId(cl_ord_id),
            orig: None,
            kind,
            venue_time: t(at),
        };
        (report, at)
    }

    fn trade(filled: &str, remaining: &str) -> ExecKind<Es> {
        ExecKind::Trade(Trade {
            exec_id: ExecId::new("T1").unwrap(),
            instrument: Es,
            side: Side::Bid,
            qty: qty(filled),
            filled: qty(filled),
            remaining: qty(remaining),
            price: px("100"),
            liquidity: Liquidity::Maker,
        })
    }

    /// What one run emitted, each with the instant it ticked at.
    struct Run {
        messages: Vec<(NanoTime, Vec<Message<Es>>)>,
        reports: Vec<(NanoTime, Vec<Report<Es>>)>,
    }

    fn flat<T: Copy + Default>(rows: Vec<(NanoTime, Burst<T>)>) -> Vec<(NanoTime, Vec<T>)> {
        rows.into_iter()
            .map(|(at, burst)| (at, burst.to_vec()))
            .collect()
    }

    fn run(requests: Vec<(Request<Es>, u64)>, executions: Vec<(ExecReport<Es>, u64)>) -> Run {
        let g = GraphBuilder::new();
        let requests = g.replay(requests.into_iter().map(|(r, at)| (r, t(at))));
        let executions = g.replay(executions.into_iter().map(|(r, at)| (r, t(at))));
        let (messages, reports) = requests.fix_bridge(false, Epoch::ZERO, &executions);
        let messages = messages.with_time().accumulate();
        let reports = reports.with_time().accumulate();
        let mut runner = g.build();
        runner
            .run(RunMode::HistoricalFrom(NanoTime::ZERO), RunFor::Forever)
            .unwrap();
        Run {
            messages: flat(runner.value(&messages)),
            reports: flat(runner.value(&reports)),
        }
    }

    /// A place goes out under the bridge's first id; the venue's `New` on it
    /// comes back as an ack on the OMS's own id.
    #[test]
    fn a_place_goes_out_and_its_new_comes_back_as_the_oms_ack() {
        let run = run(vec![(place(7), 10)], vec![report(1, ExecKind::New, 11)]);
        assert_eq!(run.messages, [(t(10), vec![new(1)])]);
        assert_eq!(
            run.reports,
            [(
                t(11),
                vec![Report::Ack(Ack {
                    order: ClientOrderId(7),
                    venue_id: VenueId::new("1").unwrap(),
                    venue_time: Some(t(11)),
                    price: Some(px("100")),
                    remaining: Some(qty("2")),
                    recv_time: t(11),
                })]
            )]
        );
    }

    /// A partial fill and an amend in one instant: the fill is received
    /// first, so the replace states the venue's `CumQty` plus what should
    /// show. Rendered the other way round it would state 2, and the venue
    /// would rest 1.5 where the OMS believes 2.
    #[test]
    fn an_execution_is_received_before_the_same_instants_request() {
        let amend = Request::Amend(Amend {
            order: ClientOrderId(7),
            instrument: Es,
            price: px("100"),
            qty: qty("2"),
            ..Amend::default()
        });
        let run = run(
            vec![(place(7), 10), (amend, 20)],
            vec![
                report(1, ExecKind::New, 11),
                report(1, trade("0.5", "1.5"), 20),
            ],
        );
        assert_eq!(run.messages.len(), 2, "{:?}", run.messages);
        let (at, sent) = &run.messages[1];
        assert_eq!(*at, t(20));
        assert!(
            matches!(
                sent.as_slice(),
                [Message::Replace { cl_ord_id: ClOrdId(2), orig: ClOrdId(1), qty: total, .. }]
                    if *total == qty("2.5")
            ),
            "{sent:?}"
        );
        let (at, reports) = &run.reports[1];
        assert_eq!(*at, t(20));
        assert!(
            matches!(
                reports.as_slice(),
                [Report::Fill(Fill {
                    order: ClientOrderId(7),
                    ..
                })]
            ),
            "{reports:?}"
        );
    }

    /// An order filled to nothing and a cancel of it in one instant: the
    /// fill retires it, so the cancel is refused at once as unknown and
    /// never sent — and the refusal follows the fill.
    #[test]
    fn a_request_for_an_order_retired_in_the_same_instant_is_refused_after_its_report() {
        let run = run(
            vec![(place(7), 10), (Request::Cancel(ClientOrderId(7)), 20)],
            vec![report(1, ExecKind::New, 11), report(1, trade("2", "0"), 20)],
        );
        assert_eq!(run.messages, [(t(10), vec![new(1)])]);
        let (at, reports) = &run.reports[1];
        assert_eq!(*at, t(20));
        assert!(
            matches!(
                reports.as_slice(),
                [
                    Report::Fill(Fill {
                        order: ClientOrderId(7),
                        ..
                    }),
                    Report::Reject(Reject {
                        order: Some(ClientOrderId(7)),
                        reason: RejectReason::UnknownOrder,
                        ..
                    })
                ]
            ),
            "{reports:?}"
        );
    }

    /// Sent from the node: a message the transport refuses is answered as
    /// the venue's refusal in the same instant — a place forgotten, a cancel
    /// refused with the order still resting — and one it takes is not.
    #[test]
    fn a_message_the_transport_refuses_is_refused_in_the_same_instant() {
        let g = GraphBuilder::new();
        let requests = g.replay([
            (place(7), t(10)),
            (place(8), t(10)),
            (Request::Cancel(ClientOrderId(7)), t(20)),
        ]);
        let executions = g.replay([(report(1, ExecKind::New, 11).0, t(11))]);
        let sent = std::rc::Rc::new(std::cell::RefCell::new(Vec::new()));
        let log = sent.clone();
        // Takes the first place, refuses everything after it.
        let reports = requests
            .fix_bridge_sending(
                false,
                Epoch::ZERO,
                &executions,
                move |m: &Message<Es>, at| {
                    log.borrow_mut().push((at, *m));
                    if log.borrow().len() == 1 {
                        Ok(())
                    } else {
                        Err(RejectReason::RateLimited)
                    }
                },
            )
            .with_time()
            .accumulate();
        let mut runner = g.build();
        runner
            .run(RunMode::HistoricalFrom(NanoTime::ZERO), RunFor::Forever)
            .unwrap();

        assert_eq!(sent.borrow().len(), 3, "{:?}", sent.borrow());
        let reports = flat(runner.value(&reports));
        assert_eq!(reports.len(), 3, "{reports:?}");
        assert_eq!(reports[0].0, t(10));
        assert!(
            matches!(
                reports[0].1.as_slice(),
                [Report::Reject(Reject {
                    order: Some(ClientOrderId(8)),
                    reason: RejectReason::RateLimited,
                    ..
                })]
            ),
            "{reports:?}"
        );
        assert_eq!(reports[2].0, t(20));
        assert!(
            matches!(
                reports[2].1.as_slice(),
                [Report::Reject(Reject {
                    order: Some(ClientOrderId(7)),
                    ..
                })]
            ),
            "{reports:?}"
        );
    }
}
