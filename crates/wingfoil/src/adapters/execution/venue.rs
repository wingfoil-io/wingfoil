//! The swap point: [`Venue`], one implementation per run mode —
//! a simulator for a backtest, a socket for a live book — which the
//! strategy graph sends its [`Request`]s into and cannot tell apart. Nothing
//! between the feed and the venue may ask which it got, and a trait object
//! is that rule made unbreakable rather than merely written down.
//!
//! # Why the trait lives with the edge
//!
//! A trait over [`Request`] and [`Report`] is a statement about that
//! vocabulary rather than about either implementation, which is what lets
//! both live where their state does — the fill model with the simulator,
//! the socket with the venue's integration — neither knowing the other.
//!
//! # Why a session carries more than the reports
//!
//! A [`Session`] is seven forward edges, one per audience: the reports for the
//! OMS; settlements, cashflows and the venue's holdings for the position fold
//! and its reconciliation; the account for the risk limits, which are
//! handed an equity and a margin rather than computing them — live they are
//! the venue's own arithmetic, not a model of it; and market maker
//! protection trips, the venue acting on the book on its own authority; and
//! the trading state, for the OMS. What each carries is a plain value in
//! [`edge`](crate::adapters::execution::edge), beside the requests and reports; this module is
//! only the wiring.
//!
//! # The loop this sits in, and where it is cut
//!
//! Requests come *out* of the strategy graph and reports go back *in*, so the
//! two together are a cycle and something has to break it. The cut is on the
//! request wire, in the caller's graph, which means a [`Venue`] sees a burst
//! of requests one engine instant after the OMS emitted it. That is where the
//! delay belongs: an order takes time to reach a venue, and a backtest whose
//! orders arrived instantly could fill on a print its own order provoked. An
//! implementation therefore does **not** wire a feedback edge of its own.
//!
//! Closed, the loop is one graph and one cut:
//!
//! ```ignore
//! let (landed, cut) = g.feedback::<Burst<Request<I>>>();
//! let session = venue.wire(&landed);
//! let (requests, pacing) =
//!     desired.oms(config, epoch, &session.reports, &session.trading, &cancel_all, &sweep);
//! let _sent = requests.feedback(&cut);
//! ```
//!
//! The OMS's reports input is downstream of the cut's source, and its
//! requests are upstream of the cut's sink, so the graph stays acyclic. The
//! test harness ships as a `Venue` (`testing::SimVenue`, feature
//! `execution-testing`), so a strategy is run against it exactly as above;
//! `tests/execution_fix_venue.rs` does.

use crate::prelude::*;

use crate::adapters::execution::edge::{
    Account, Cashflow, Holdings, MmpTrip, Report, Request, TradingState,
};
use crate::adapters::execution::order::{Fill, Instrument};

/// What a venue gives back for the requests it is sent.
///
/// Seven streams rather than one enum, because they are seven rates and —
/// more to the point — seven audiences. Reports answer the orders the OMS
/// sent and only it reads them all; settlements answer no order and only
/// the position fold reads them; the account is re-read whenever the book is
/// repriced and only the risk limits read it; the holdings are the venue's
/// own view of the positions and only reconciliation reads them; a cashflow is
/// money the position fold books and nothing decides on; an MMP trip is the
/// venue saying it pulled the quote book, which only the limits read; the
/// trading state says when the OMS may send at all. Folding
/// them into one edge would wake the OMS on every mark and hand it
/// executions it never asked for.
pub struct Session<I: Instrument> {
    /// What became of the requests — acks, fills, rejects, cancels and
    /// expiries, in the venue's own order within an instant.
    ///
    /// Settlement is **not** among them — see
    /// [`settlements`](Self::settlements).
    pub reports: Stream<Burst<Report<I>>>,
    /// Executions against no order of ours: a contract that reached its cut
    /// and was closed by the venue at the delivery price.
    ///
    /// Its own edge rather than a [`Report::Fill`], because the OMS keys on
    /// the orders it sent and a settlement answers none of them — it would
    /// be counted as an execution on an order the OMS has never heard of,
    /// and order reconciliation would later read a working set made false
    /// on purpose. So the position fold applies these and the OMS never
    /// sees them, which is the whole difference between a [`Fill`] and a
    /// [`Report`].
    pub settlements: Stream<Burst<Fill<I>>>,
    /// What the account is worth and how much of it margin is holding.
    pub account: Stream<Account>,
    /// What the venue says it holds — the truth the position fold is
    /// reconciled against ([`reconcile`](crate::adapters::execution::reconcile)).
    ///
    /// A simulated venue states its own book, which agrees with the
    /// strategy's by construction — both are fed the same reports — so a
    /// backtest proves the reconciliation is *quiet*, never that it catches
    /// anything.
    pub holdings: Stream<Holdings<I>>,
    /// Money moved on a position with no execution behind it — funding, a
    /// coupon, a dividend, a borrow fee — booked by the position fold as its
    /// own line.
    pub cashflows: Stream<Burst<Cashflow<I>>>,
    /// The venue's market maker protection firing ([`MmpTrip`]): the quote
    /// book was hit harder, inside the protection's interval, than its
    /// limits allow. The risk limits latch on it. Silent on a venue run
    /// without protection.
    pub mmp: Stream<Burst<MmpTrip>>,
    /// The venue's trading state, where it has one: the OMS holds everything
    /// but cancels while it is not open. A continuous venue never ticks it.
    pub trading: Stream<TradingState>,
}

impl<I: Instrument + 'static> Session<I> {
    /// A session that answers with reports and nothing else: every other
    /// stream is silent — what a venue that states no holdings, cashflows,
    /// account, protection or trading state hands back, and the base a
    /// fuller one is built on by struct update:
    ///
    /// ```ignore
    /// Session {
    ///     reports: reports.clone(),
    ///     account,
    ///     ..Session::quiet(reports)
    /// }
    /// ```
    ///
    /// Each silent stream is derived from `reports` and never ticks, so a
    /// minimal venue is one line and keeps the seven-stream shape: every
    /// reader of a session wires the same way whatever the venue states.
    /// A silent stream is a node that runs when the reports tick and emits
    /// nothing — one that struct update overrides stays wired, and costs
    /// that and no more.
    pub fn quiet(reports: Stream<Burst<Report<I>>>) -> Session<I> {
        Session {
            settlements: reports.filter_map(|_: &Burst<Report<I>>| None),
            account: reports.filter_map(|_: &Burst<Report<I>>| None),
            holdings: reports.filter_map(|_: &Burst<Report<I>>| None),
            cashflows: reports.filter_map(|_: &Burst<Report<I>>| None),
            mmp: reports.filter_map(|_: &Burst<Report<I>>| None),
            trading: reports.filter_map(|_: &Burst<Report<I>>| None),
            reports,
        }
    }
}

/// The swap point. One implementation per run mode, and the strategy graph
/// cannot tell which it was handed.
///
/// `&self` rather than `self`: wiring reads the builder's configuration and
/// the streams it closes over, and takes nothing away.
pub trait Venue<I: Instrument> {
    /// Wire the venue into the graph `requests` belongs to.
    ///
    /// `requests` has already been through the strategy's feedback edge, so
    /// what arrives is the previous instant's burst. An implementation reads
    /// it as the orders that have just landed.
    fn wire(&self, requests: &Stream<Burst<Request<I>>>) -> Session<I>;
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::adapters::execution::edge::Ack;
    use crate::adapters::execution::order::ClientOrderId;
    use crate::{NanoTime, RunFor, RunMode};

    /// A venue on one ticker.
    #[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash, PartialOrd, Ord)]
    struct Ticker;

    fn ack(order: u64, at: u64) -> (Report<Ticker>, NanoTime) {
        let report = Report::Ack(Ack {
            order: ClientOrderId(order),
            recv_time: NanoTime::new(at),
            ..Ack::default()
        });
        (report, NanoTime::new(at))
    }

    /// A quiet session passes its reports through as they came, and not one
    /// of its other six streams ticks — however often the reports do.
    #[test]
    fn a_quiet_session_ticks_its_reports_and_nothing_else() {
        let g = GraphBuilder::new();
        let rows = [ack(1, 10), ack(2, 20), ack(3, 20)];
        let reports = g.replay_results(rows.into_iter().map(Ok));
        let session = Session::quiet(reports);

        let reports = session.reports.accumulate();
        let settlements = session.settlements.count();
        let account = session.account.count();
        let holdings = session.holdings.count();
        let funding = session.cashflows.count();
        let mmp = session.mmp.count();
        let trading = session.trading.count();

        let mut runner = g.build();
        runner
            .run(RunMode::HistoricalFrom(NanoTime::ZERO), RunFor::Forever)
            .unwrap();

        let reports = runner.value(&reports);
        assert_eq!(reports.len(), 2, "two instants: {reports:?}");
        assert_eq!(reports[0].as_slice(), [ack(1, 10).0]);
        assert_eq!(reports[1].as_slice(), [ack(2, 20).0, ack(3, 20).0]);
        let silent = [
            runner.value(&settlements),
            runner.value(&account),
            runner.value(&holdings),
            runner.value(&funding),
            runner.value(&mmp),
            runner.value(&trading),
        ];
        assert_eq!(
            silent, [0; 6],
            "settlements, account, holdings, cashflows, mmp, trading"
        );
    }
}
