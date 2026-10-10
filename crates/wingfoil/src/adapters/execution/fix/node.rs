//! The replace chain as a graph node: [`ReplaceChainOp`], wired through
//! [`FixOps`].
//!
//! [`ReplaceChain`] has two mutating entry points — [`receive`](ReplaceChain::receive)
//! and [`send`](ReplaceChain::send) — and, as with the OMS, the order they
//! are called in within one instant is part of what they mean. It is the
//! node's, stated once:
//!
//! 1. **executions** — [`ReplaceChain::receive`], the whole burst in order;
//! 2. **requests** — [`ReplaceChain::send`], the whole burst in order.
//!
//! Executions first, because a replace states the order's total as what has
//! filled plus what should show: a fill the venue has already reported has
//! to be counted before an amend in the same instant is rendered, or the
//! venue rests less than the OMS believes. And an order a terminal report
//! retired in this instant is gone by the time a request names it, so the
//! request is answered at once as unknown rather than sent to be refused.
//!
//! The reports out follow the same order: the venue's, translated, then the
//! refusals [`send`](ReplaceChain::send) answers at once.
//!
//! The executions are the venue's — a session's inbound side, a source —
//! and the requests have already crossed the caller's feedback cut, so the
//! node sits on the forward side of it and adds none. A venue that answers
//! in the instant it is sent to, as the test venue does, cannot feed this
//! node without a second cycle; it holds the chain beside itself instead
//! (`testing::SimVenue`) and calls it in the same order.

use std::marker::PhantomData;

use crate::op::{Activation, Ctx, Op, Tick};
use crate::prelude::*;
use anyhow::Result;

use super::{ExecReport, Message, ReplaceChain};
use crate::adapters::execution::edge::{Report, Request};
use crate::adapters::execution::order::{Epoch, Instrument};

/// The chain as an op: a [`ReplaceChain`] held as node state, fed the
/// venue's executions and then the OMS's requests, in the order the
/// [module docs](self) state, emitting the messages to send and the
/// reports for the OMS.
///
/// Wire it with [`FixOps`]; the op itself is public so a compiled graph can
/// name it.
///
/// - `Cfg` — whether the venue has a mass cancel, and the [`Epoch`] the
///   chain's ids are minted in.
/// - `State` — the [`ReplaceChain`], built from the config on the first
///   cycle.
/// - `Out` — `(messages, reports)`, every cycle the node runs; either burst
///   may be empty.
pub struct ReplaceChainOp<I>(PhantomData<I>);

#[crate::op(build = replace_chain_op)]
impl<I> Op for ReplaceChainOp<I>
where
    I: Instrument + 'static,
{
    type Cfg = (bool, Epoch);
    type State = Option<ReplaceChain<I>>;
    type In<'a> = (
        (&'a Burst<ExecReport<I>>, bool),
        (&'a Burst<Request<I>>, bool),
    );
    type Out = (Burst<Message<I>>, Burst<Report<I>>);
    const ACTIVATION: Activation = Activation::NONE;

    fn cycle(
        cfg: &mut (bool, Epoch),
        state: &mut Option<ReplaceChain<I>>,
        input: Self::In<'_>,
        ctx: &mut Ctx<'_>,
    ) -> Result<Tick<(Burst<Message<I>>, Burst<Report<I>>)>> {
        let (mass_cancel, epoch) = *cfg;
        let chain = state.get_or_insert_with(|| ReplaceChain::new(mass_cancel, epoch));
        let ((executions, executed), (requests, requested)) = input;
        let now = ctx.time();
        let mut messages: Burst<Message<I>> = Burst::new();
        let mut reports: Burst<Report<I>> = Burst::new();
        if executed {
            reports.extend(chain.receive(now, executions));
        }
        if requested {
            let (sent, refused) = chain.send(now, requests);
            messages.extend(sent);
            reports.extend(refused);
        }
        Ok(Tick::Value((messages, reports)))
    }
}

/// The replace chain, wired on the OMS's request stream. Out of any prelude:
/// `use wingfoil::adapters::execution::fix::FixOps;`.
///
/// Two inputs, applied within one instant in this order whichever of them
/// ticked:
///
/// 1. `executions` — the venue's execution reports, translated onto the
///    OMS's ids;
/// 2. `self` — the OMS's requests, already through the caller's feedback
///    cut, rendered as messages — or refused at once where they name an
///    order the chain no longer holds.
///
/// Returns the messages for the venue, ticking only on a cycle that sent
/// any, and the reports for the OMS's `reports` input — the translations,
/// then the refusals — ticking only on a cycle that had any.
pub trait FixOps<I: Instrument + 'static> {
    /// The chain for a venue that has a mass cancel or not, minting under
    /// `epoch` (see [`ReplaceChain::new`]): see the trait docs for the
    /// inputs and the order they are applied in.
    #[must_use = "a dropped stream stays wired and cycles every tick, producing an unread value"]
    fn replace_chain(
        &self,
        mass_cancel: bool,
        epoch: Epoch,
        executions: &Stream<Burst<ExecReport<I>>>,
    ) -> (Stream<Burst<Message<I>>>, Stream<Burst<Report<I>>>);
}

impl<I: Instrument + 'static> FixOps<I> for Stream<Burst<Request<I>>> {
    fn replace_chain(
        &self,
        mass_cancel: bool,
        epoch: Epoch,
        executions: &Stream<Burst<ExecReport<I>>>,
    ) -> (Stream<Burst<Message<I>>>, Stream<Burst<Report<I>>>) {
        let executions = executions.handle();
        let node = self.wire(move |b, requests| {
            b.replace_chain_op(executions, requests, (mass_cancel, epoch))
        });
        let messages = node.filter_map(|(messages, _): &(Burst<Message<I>>, Burst<Report<I>>)| {
            (!messages.is_empty()).then(|| messages.clone())
        });
        let reports = node.filter_map(|(_, reports): &(Burst<Message<I>>, Burst<Report<I>>)| {
            (!reports.is_empty()).then(|| reports.clone())
        });
        (messages, reports)
    }
}

#[cfg(test)]
mod tests {
    use crate::adapters::market::{Px, Qty, Side};
    use crate::{NanoTime, RunFor, RunMode};

    use super::*;
    use crate::adapters::execution::edge::{Ack, Amend, Reject, RejectReason};
    use crate::adapters::execution::exec_id::{ExecId, VenueId};
    use crate::adapters::execution::fix::{ClOrdId, ExecKind, NewOrder, Trade};
    use crate::adapters::execution::order::{
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
        let requests = g.replay_results(requests.into_iter().map(|(r, at)| Ok((r, t(at)))));
        let executions = g.replay_results(executions.into_iter().map(|(r, at)| Ok((r, t(at)))));
        let (messages, reports) = requests.replace_chain(false, Epoch::ZERO, &executions);
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

    /// A place goes out under the chain's first id; the venue's `New` on it
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
}
