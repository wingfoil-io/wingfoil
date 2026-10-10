//! The OMS as a graph node: [`OmsOp`], wired through [`OmsOps`].
//!
//! The short form names the inputs a caller has and leaves out the rest:
//!
//! ```
//! use wingfoil::prelude::*;
//! use wingfoil::adapters::execution::edge::{Report, Request};
//! use wingfoil::adapters::execution::oms::{Config, Desired, OmsOps, Pacing};
//! use wingfoil::adapters::execution::order::{Epoch, Instrument};
//!
//! fn quote<I: Instrument + 'static>(
//!     desired: &Stream<Burst<Desired<I>>>,
//!     reports: &Stream<Burst<Report<I>>>,
//!     clock: &Stream<()>,
//!     config: Config,
//! ) -> (Stream<Burst<Request<I>>>, Stream<Pacing>) {
//!     desired.wire_oms(config, Epoch::ZERO, reports).sweep(clock).build()
//! }
//! ```
//!
//! `.trading(..)`, `.cancel_all(..)` and `.reading(..)` add the optional
//! inputs and the look at the OMS ([`OmsWiring`]); an input left out is wired
//! as a stream that never ticks. The sweep is the one input that cannot be
//! left out: without `.sweep(..)` there is no `build`. [`OmsOps::oms`] and
//! [`OmsOps::oms_reading`] take every input positionally and wire the same
//! node.
//!
//! [`Oms`] is a fold with four mutating entry points — [`apply`](Oms::apply),
//! [`trading`](Oms::trading), [`cancel_all`](Oms::cancel_all) and
//! [`diff`](Oms::diff) — and the order they are called in within one instant
//! is part of what it means. A report that freed a slot has to land before
//! the decision is diffed against it, or the re-quote waits an instant for
//! nothing; a cancel-all has to go out ahead of what the same instant then
//! places. Left to each caller, every graph re-derived that order by hand.
//! Here it is the node's, stated once:
//!
//! 1. **reports** — [`Oms::apply`], the whole burst in order;
//! 2. **trading** — [`Oms::trading`], the venue's trading state;
//! 3. **cancel-all** — [`Oms::cancel_all`], its request at the **head** of
//!    the burst;
//! 4. **desired** — the burst of [`Desired`]s, collected;
//! 5. **sweep** — nothing: it exists so the node runs with nothing new;
//!
//! then [`Oms::diff`] at the engine time, on **every** cycle the node runs —
//! the sweep's included, which is what judges staleness and re-drives what the
//! order-rate budget held back (the OMS's module docs, "Re-driven by the same
//! clock as staleness"). The short form does not change that order: it only
//! fills the slots a caller left empty.
//!
//! A graph drives the OMS through this node and nothing else. The pure
//! methods stay for tests and for tools that are not graphs.

use std::marker::PhantomData;

use crate::op::{Activation, Ctx, Op, Tick};
use crate::prelude::*;
use anyhow::Result;

use super::{Config, Desired, Oms, Pacing};
use crate::adapters::execution::edge::{Report, Request, TradingState};
use crate::adapters::execution::order::{Epoch, Instrument};

/// The OMS as an op: an [`Oms`] held as node state, fed its five inputs
/// in the fixed order the [module docs](self) state, emitting what the
/// instant's diff sent, the pacing after it, and what `read` makes of the
/// OMS once it has.
///
/// Wire it with [`OmsOps`]; the op itself is public so a compiled graph can
/// name it.
///
/// - `Cfg` — the OMS's [`Config`], the process's [`Epoch`] its ids are
///   minted in, and `read`, a look at the OMS after the diff.
/// - `State` — the [`Oms`], built from the config on the first cycle.
/// - `Out` — `(requests, pacing, read)`, every cycle the node runs; the
///   request burst may be empty.
pub struct OmsOp<I, R, F>(PhantomData<(I, R, F)>);

#[crate::op(build = oms_op)]
impl<I, R, F> Op for OmsOp<I, R, F>
where
    I: Instrument + 'static,
    R: Clone + 'static,
    F: Fn(&Oms<I>) -> R + 'static,
{
    type Cfg = (Config, Epoch, F);
    type State = Option<Oms<I>>;
    type In<'a> = (
        (&'a Burst<Report<I>>, bool),
        (&'a TradingState, bool),
        (&'a (), bool),
        (&'a Burst<Desired<I>>, bool),
        (&'a (), bool),
    );
    type Out = (Burst<Request<I>>, Pacing, R);
    const ACTIVATION: Activation = Activation::NONE;

    fn cycle(
        cfg: &mut (Config, Epoch, F),
        state: &mut Option<Oms<I>>,
        input: Self::In<'_>,
        ctx: &mut Ctx<'_>,
    ) -> Result<Tick<(Burst<Request<I>>, Pacing, R)>> {
        let (config, epoch, read) = cfg;
        let oms = state.get_or_insert_with(|| Oms::resumed(*config, *epoch));
        // The sweep's flag is not read: it changes nothing, and the diff
        // below runs on every cycle whichever input woke the node.
        let ((reports, reported), (trading, stated), (_, pull), (desired, wanted), _sweep) = input;
        let now = ctx.time();
        let mut requests: Burst<Request<I>> = Burst::new();
        if reported {
            oms.apply(reports);
        }
        if stated {
            oms.trading(*trading);
        }
        if pull {
            requests.extend(oms.cancel_all(now));
        }
        let desired: &[Desired<I>] = if wanted { desired.as_slice() } else { &[] };
        requests.extend(oms.diff(now, desired));
        let pacing = oms.pacing();
        let read = read(oms);
        Ok(Tick::Value((requests, pacing, read)))
    }
}

/// The OMS node, wired on a stream of desireds. Out of any prelude:
/// `use wingfoil::adapters::execution::oms::OmsOps;`.
///
/// Start from [`wire_oms`](Self::wire_oms), which takes what every OMS needs
/// and returns an [`OmsWiring`] for the rest:
///
/// ```ignore
/// let (requests, pacing) = desired
///     .wire_oms(config, epoch, &reports)
///     .sweep(&clock)            // required: there is no `build` without it
///     .trading(&trading_state)  // optional: a venue with auctions or halts
///     .cancel_all(&kill)        // optional: a kill switch
///     .build();
/// ```
///
/// Five inputs, applied within one instant in this order whichever of them
/// ticked:
///
/// 1. `reports` — what the venue said of what was sent, applied in order;
/// 2. `trading` — the venue's trading state; while it is not open, only
///    cancels go out. A continuous venue never ticks it, and the short form
///    leaves it out;
/// 3. `cancel_all` — pull everything: its request goes at the **head** of the
///    burst, so the venue pulls everything and then sees whatever this
///    instant's decision wanted placed. Left out, nothing pulls;
/// 4. `self` — the desireds, merged into what the OMS remembers;
/// 5. `sweep` — a clock, changing nothing: the diff runs on it anyway, which
///    is what withdraws a desired past `max_desired_age` with nothing new
///    arriving, and what sends a request the order-rate budget held back once
///    a token has refilled. Never left out: which clock that is is a choice.
///
/// Then the diff, at the engine time, on every cycle the node runs.
///
/// Returns the requests — ticking only on a cycle that sent any — and the
/// [`Pacing`] on every cycle the node runs.
pub trait OmsOps<I: Instrument + 'static> {
    /// The OMS on `config`, minting ids in `epoch` and applying `reports`:
    /// the start of the short form. Name the sweep with
    /// [`sweep`](OmsWiring::sweep), add whichever optional inputs the graph
    /// has, then `build`.
    fn wire_oms(
        &self,
        config: Config,
        epoch: Epoch,
        reports: &Stream<Burst<Report<I>>>,
    ) -> OmsWiring<I>;

    /// The OMS on `config`, minting ids in `epoch`, every input named: see
    /// the trait docs for the inputs and the order they are applied in. A
    /// caller without a `trading` or `cancel_all` stream wants
    /// [`wire_oms`](Self::wire_oms).
    #[must_use = "a dropped stream stays wired and cycles every tick, producing an unread value"]
    fn oms(
        &self,
        config: Config,
        epoch: Epoch,
        reports: &Stream<Burst<Report<I>>>,
        trading: &Stream<TradingState>,
        cancel_all: &Stream<()>,
        sweep: &Stream<()>,
    ) -> (Stream<Burst<Request<I>>>, Stream<Pacing>);

    /// [`oms`](Self::oms), and a third stream: what `read` makes of the OMS
    /// once each cycle's diff is done, every cycle the node runs. The short
    /// form is [`OmsWiring::reading`].
    ///
    /// A look, never a lever — `read` has the OMS by shared reference. It is
    /// for a caller whose decision depends on what is in flight (a flatten
    /// that stops once nothing is) and for a test that asserts on a slot.
    #[allow(clippy::too_many_arguments)]
    #[must_use = "a dropped stream stays wired and cycles every tick, producing an unread value"]
    fn oms_reading<R, F>(
        &self,
        config: Config,
        epoch: Epoch,
        reports: &Stream<Burst<Report<I>>>,
        trading: &Stream<TradingState>,
        cancel_all: &Stream<()>,
        sweep: &Stream<()>,
        read: F,
    ) -> (Stream<Burst<Request<I>>>, Stream<Pacing>, Stream<R>)
    where
        R: Clone + Default + 'static,
        F: Fn(&Oms<I>) -> R + 'static;
}

impl<I: Instrument + 'static> OmsOps<I> for Stream<Burst<Desired<I>>> {
    fn wire_oms(
        &self,
        config: Config,
        epoch: Epoch,
        reports: &Stream<Burst<Report<I>>>,
    ) -> OmsWiring<I> {
        OmsWiring {
            desired: self.clone(),
            config,
            epoch,
            reports: reports.clone(),
            trading: None,
            cancel_all: None,
            sweep: Unswept,
            read: (),
        }
    }

    fn oms(
        &self,
        config: Config,
        epoch: Epoch,
        reports: &Stream<Burst<Report<I>>>,
        trading: &Stream<TradingState>,
        cancel_all: &Stream<()>,
        sweep: &Stream<()>,
    ) -> (Stream<Burst<Request<I>>>, Stream<Pacing>) {
        let node = wire(
            self,
            config,
            epoch,
            reports,
            trading,
            cancel_all,
            sweep,
            |_| (),
        );
        (requests(&node), pacing(&node))
    }

    fn oms_reading<R, F>(
        &self,
        config: Config,
        epoch: Epoch,
        reports: &Stream<Burst<Report<I>>>,
        trading: &Stream<TradingState>,
        cancel_all: &Stream<()>,
        sweep: &Stream<()>,
        read: F,
    ) -> (Stream<Burst<Request<I>>>, Stream<Pacing>, Stream<R>)
    where
        R: Clone + Default + 'static,
        F: Fn(&Oms<I>) -> R + 'static,
    {
        let node = wire(
            self, config, epoch, reports, trading, cancel_all, sweep, read,
        );
        (requests(&node), pacing(&node), reading(&node))
    }
}

/// The OMS node's inputs, named, from [`OmsOps::wire_oms`]: the short form
/// of [`OmsOps::oms`] for a caller that has only some of them.
///
/// - [`sweep`](Self::sweep) — **required**; `build` exists only once it is
///   named. It is what withdraws a stale desired and re-drives what the
///   order-rate budget held, so its clock is the caller's choice to make.
/// - [`trading`](Self::trading) — optional; left out, the OMS stays open, as
///   it does on a continuous venue that never states one.
/// - [`cancel_all`](Self::cancel_all) — optional; left out, nothing pulls.
/// - [`reading`](Self::reading) — optional; adds a third output stream, what
///   a look at the OMS makes of it after each cycle's diff.
///
/// An input left out is wired as a stream that never ticks, so what `build`
/// wires is the node [`OmsOps::oms`] wires when handed never-ticking
/// streams — same inputs, same order, same requests at the same instants.
/// `S` is [`Unswept`] until the sweep is named; `F` is `()` until a reading
/// is.
#[must_use = "an OmsWiring wires nothing until `build` is called"]
pub struct OmsWiring<I: Instrument + 'static, S = Unswept, F = ()> {
    desired: Stream<Burst<Desired<I>>>,
    config: Config,
    epoch: Epoch,
    reports: Stream<Burst<Report<I>>>,
    trading: Option<Stream<TradingState>>,
    cancel_all: Option<Stream<()>>,
    sweep: S,
    read: F,
}

/// An [`OmsWiring`] whose sweep is not named yet: it has no `build`.
#[derive(Clone, Copy, Debug)]
pub struct Unswept;

/// An [`OmsWiring`]'s look at the OMS, from [`OmsWiring::reading`].
pub struct Reading<F>(F);

impl<I: Instrument + 'static, S, F> OmsWiring<I, S, F> {
    /// The clock the diff runs on with nothing new arriving: what withdraws
    /// a desired past `max_desired_age`, and what sends a request the
    /// order-rate budget held back once a token has refilled. Required.
    pub fn sweep(self, sweep: &Stream<()>) -> OmsWiring<I, Stream<()>, F> {
        OmsWiring {
            desired: self.desired,
            config: self.config,
            epoch: self.epoch,
            reports: self.reports,
            trading: self.trading,
            cancel_all: self.cancel_all,
            sweep: sweep.clone(),
            read: self.read,
        }
    }

    /// The venue's trading state; while it is not open, only cancels go out.
    /// Leave it out on a continuous venue, which never states one.
    pub fn trading(mut self, trading: &Stream<TradingState>) -> Self {
        self.trading = Some(trading.clone());
        self
    }

    /// Pull everything on each tick: the cancel-all goes at the head of the
    /// instant's burst, ahead of whatever the same instant places.
    pub fn cancel_all(mut self, cancel_all: &Stream<()>) -> Self {
        self.cancel_all = Some(cancel_all.clone());
        self
    }

    /// The optional edges, each one left out a stream that never ticks.
    fn optional(&self) -> (Stream<TradingState>, Stream<()>) {
        let graph = self.desired.graph();
        let trading = match &self.trading {
            Some(trading) => trading.clone(),
            // Never ticks, so the state is never applied: the OMS starts open.
            None => graph.never().map(|(): &()| TradingState::Open),
        };
        let cancel_all = match &self.cancel_all {
            Some(cancel_all) => cancel_all.clone(),
            None => graph.never(),
        };
        (trading, cancel_all)
    }
}

impl<I: Instrument + 'static, S> OmsWiring<I, S, ()> {
    /// A look at the OMS once each cycle's diff is done, as a third stream
    /// from `build` — [`OmsOps::oms_reading`]'s `read`. A look, never a
    /// lever: `read` has the OMS by shared reference.
    pub fn reading<R, F>(self, read: F) -> OmsWiring<I, S, Reading<F>>
    where
        R: Clone + Default + 'static,
        F: Fn(&Oms<I>) -> R + 'static,
    {
        OmsWiring {
            desired: self.desired,
            config: self.config,
            epoch: self.epoch,
            reports: self.reports,
            trading: self.trading,
            cancel_all: self.cancel_all,
            sweep: self.sweep,
            read: Reading(read),
        }
    }
}

impl<I: Instrument + 'static> OmsWiring<I, Stream<()>, ()> {
    /// Wire the node: the requests, ticking only on a cycle that sent any,
    /// and the [`Pacing`] on every cycle the node runs — as [`OmsOps::oms`].
    #[must_use = "a dropped stream stays wired and cycles every tick, producing an unread value"]
    pub fn build(self) -> (Stream<Burst<Request<I>>>, Stream<Pacing>) {
        let (trading, cancel_all) = self.optional();
        let node = wire(
            &self.desired,
            self.config,
            self.epoch,
            &self.reports,
            &trading,
            &cancel_all,
            &self.sweep,
            |_| (),
        );
        (requests(&node), pacing(&node))
    }
}

impl<I, R, F> OmsWiring<I, Stream<()>, Reading<F>>
where
    I: Instrument + 'static,
    R: Clone + Default + 'static,
    F: Fn(&Oms<I>) -> R + 'static,
{
    /// Wire the node: the requests, the [`Pacing`], and what the
    /// [`reading`](OmsWiring::reading) makes of the OMS every cycle the node
    /// runs — as [`OmsOps::oms_reading`].
    #[must_use = "a dropped stream stays wired and cycles every tick, producing an unread value"]
    pub fn build(self) -> (Stream<Burst<Request<I>>>, Stream<Pacing>, Stream<R>) {
        let (trading, cancel_all) = self.optional();
        let node = wire(
            &self.desired,
            self.config,
            self.epoch,
            &self.reports,
            &trading,
            &cancel_all,
            &self.sweep,
            self.read.0,
        );
        (requests(&node), pacing(&node), reading(&node))
    }
}

/// The node itself, its edges in the order [`OmsOp`] reads them.
#[allow(clippy::too_many_arguments)]
fn wire<I, R, F>(
    desired: &Stream<Burst<Desired<I>>>,
    config: Config,
    epoch: Epoch,
    reports: &Stream<Burst<Report<I>>>,
    trading: &Stream<TradingState>,
    cancel_all: &Stream<()>,
    sweep: &Stream<()>,
    read: F,
) -> Stream<(Burst<Request<I>>, Pacing, R)>
where
    I: Instrument + 'static,
    R: Clone + Default + 'static,
    F: Fn(&Oms<I>) -> R + 'static,
{
    let (reports, trading, cancel_all, sweep) = (
        reports.handle(),
        trading.handle(),
        cancel_all.handle(),
        sweep.handle(),
    );
    desired.wire(move |b, desired| {
        b.oms_op(
            reports,
            trading,
            cancel_all,
            desired,
            sweep,
            (config, epoch, read),
        )
    })
}

/// The requests, on a cycle that sent any.
fn requests<I, R>(node: &Stream<(Burst<Request<I>>, Pacing, R)>) -> Stream<Burst<Request<I>>>
where
    I: Instrument + 'static,
    R: Clone + Default + 'static,
{
    node.filter_map(|(requests, _, _): &(Burst<Request<I>>, Pacing, R)| {
        (!requests.is_empty()).then(|| requests.clone())
    })
}

/// The pacing, every cycle.
fn pacing<I, R>(node: &Stream<(Burst<Request<I>>, Pacing, R)>) -> Stream<Pacing>
where
    I: Instrument + 'static,
    R: Clone + Default + 'static,
{
    node.map(|(_, pacing, _): &(Burst<Request<I>>, Pacing, R)| *pacing)
}

/// What the reading made of the OMS, every cycle.
fn reading<I, R>(node: &Stream<(Burst<Request<I>>, Pacing, R)>) -> Stream<R>
where
    I: Instrument + 'static,
    R: Clone + Default + 'static,
{
    node.map(|(_, _, read): &(Burst<Request<I>>, Pacing, R)| read.clone())
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use crate::adapters::market::{Level, Px, Qty, Side};
    use crate::{NanoTime, RunFor, RunMode};

    use super::*;
    use crate::adapters::execution::edge::{Ack, Retired};
    use crate::adapters::execution::exec_id::{ExecId, VenueId};
    use crate::adapters::execution::oms::{Intent, Ladder, Lifetime, Passive, Slot};
    use crate::adapters::execution::order::{ClientOrderId, Fill, Liquidity};
    use crate::adapters::execution::rate_limit::{OrderRate, Terms};

    /// A venue listing tickers by number.
    #[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash, PartialOrd, Ord)]
    struct Ticker(u8);

    const A: Ticker = Ticker(1);
    const B: Ticker = Ticker(2);

    type Desired = super::Desired<Ticker>;
    type Report = crate::adapters::execution::edge::Report<Ticker>;
    type Request = crate::adapters::execution::edge::Request<Ticker>;

    /// Five a second, burst twenty, spent at four fifths: nothing here waits
    /// on a token.
    const RATE: OrderRate = match OrderRate::new(
        Terms { rate: 5, burst: 20 },
        Terms { rate: 5, burst: 20 },
        0.8,
    ) {
        Ok(rate) => rate,
        Err(_) => panic!("valid"),
    };

    /// A desired is believed for a second.
    const AGE: Duration = Duration::from_secs(1);

    fn config() -> Config {
        Config {
            max_desired_age: AGE,
            min_requote: Px::ZERO,
            retake: Duration::from_secs(1),
            rate: RATE,
            passive: Passive::PostOnly,
            ratio: None,
            lifetime: Lifetime::GoodTillCancel,
        }
    }

    fn t(nanos: u64) -> NanoTime {
        NanoTime::new(nanos)
    }

    fn px(text: &str) -> Px {
        Px::parse(text).unwrap()
    }

    /// The `n`th id the OMS mints, in epoch zero.
    fn id(n: u32) -> ClientOrderId {
        ClientOrderId::new(Epoch::ZERO, n)
    }

    fn bid(ticker: Ticker, price: &str, as_of: u64) -> Desired {
        Desired {
            instrument: ticker,
            bids: Ladder::one(Level::new(px(price), Qty::parse("1").unwrap())),
            asks: Ladder::none(),
            as_of: t(as_of),
            intent: Intent::Rest,
            reduce_only: false,
        }
    }

    fn ack(order: ClientOrderId, at: u64) -> Report {
        Report::Ack(Ack {
            order,
            venue_id: VenueId::new("v").unwrap(),
            venue_time: None,
            price: None,
            remaining: None,
            recv_time: t(at),
        })
    }

    /// `order`, filled to nothing.
    fn filled(order: ClientOrderId, at: u64) -> Report {
        Report::Fill(Fill {
            order,
            exec_id: ExecId::new("e").unwrap(),
            instrument: A,
            side: Side::Bid,
            qty: Qty::parse("1").unwrap(),
            remaining: Qty::ZERO,
            price: px("100"),
            liquidity: Liquidity::Maker,
            recv_time: t(at),
            ..Default::default()
        })
    }

    /// What one run is fed, each row at its engine instant.
    #[derive(Clone, Default)]
    struct Script {
        reports: Vec<(Report, u64)>,
        cancel_all: Vec<u64>,
        desired: Vec<(Desired, u64)>,
        sweep: Vec<u64>,
    }

    /// What one run emitted, each with the instant it ticked at.
    #[derive(Debug, PartialEq)]
    struct Run {
        requests: Vec<(NanoTime, Burst<Request>)>,
        pacing: Vec<(NanoTime, Pacing)>,
        /// Ticker A's bid after each cycle's diff.
        slot: Vec<(NanoTime, Slot<Ticker>)>,
    }

    fn rows<T: Clone + Default + 'static>(
        g: &GraphBuilder,
        rows: Vec<(T, u64)>,
    ) -> Stream<Burst<T>> {
        g.replay_results(rows.into_iter().map(|(row, at)| Ok((row, t(at)))))
    }

    fn beats(g: &GraphBuilder, at: Vec<u64>) -> Stream<()> {
        rows(g, at.into_iter().map(|at| ((), at)).collect()).map(|_: &Burst<()>| ())
    }

    /// Ticker A's bid: what every run reads of the OMS.
    fn slot_a(oms: &Oms<Ticker>) -> Slot<Ticker> {
        oms.slot(&A, Side::Bid)
    }

    /// `script` through the short form: no trading state, as on a
    /// continuous venue.
    fn run(script: Script) -> Run {
        let g = GraphBuilder::new();
        let reports = rows(&g, script.reports);
        let cancel_all = beats(&g, script.cancel_all);
        let desired = rows(&g, script.desired);
        let sweep = beats(&g, script.sweep);

        let (requests, pacing, slot) = desired
            .wire_oms(config(), Epoch::ZERO, &reports)
            .sweep(&sweep)
            .cancel_all(&cancel_all)
            .reading(slot_a)
            .build();
        collect(&g, requests, pacing, slot)
    }

    /// `script`, which pulls nothing, with neither optional input: through
    /// the short form leaving both out, or through the long form handed
    /// streams that never tick for both.
    fn run_bare(script: Script, long: bool) -> Run {
        assert!(script.cancel_all.is_empty(), "a bare run pulls nothing");
        let g = GraphBuilder::new();
        let reports = rows(&g, script.reports);
        let desired = rows(&g, script.desired);
        let sweep = beats(&g, script.sweep);

        let (requests, pacing, slot) = if long {
            let trading = g.never().map(|(): &()| TradingState::Open);
            desired.oms_reading(
                config(),
                Epoch::ZERO,
                &reports,
                &trading,
                &g.never(),
                &sweep,
                slot_a,
            )
        } else {
            desired
                .wire_oms(config(), Epoch::ZERO, &reports)
                .sweep(&sweep)
                .reading(slot_a)
                .build()
        };
        collect(&g, requests, pacing, slot)
    }

    /// Run the graph from zero and gather what each output ticked, and when.
    fn collect(
        g: &GraphBuilder,
        requests: Stream<Burst<Request>>,
        pacing: Stream<Pacing>,
        slot: Stream<Slot<Ticker>>,
    ) -> Run {
        let requests = requests.with_time().accumulate();
        let pacing = pacing.with_time().accumulate();
        let slot = slot.with_time().accumulate();

        let mut runner = g.build();
        runner
            .run(RunMode::HistoricalFrom(NanoTime::ZERO), RunFor::Forever)
            .unwrap();
        Run {
            requests: runner.value(&requests),
            pacing: runner.value(&pacing),
            slot: runner.value(&slot),
        }
    }

    /// `(id, ticker, price)` of a place, or a panic naming what it was.
    fn place(request: &Request) -> (ClientOrderId, Ticker, Px) {
        match request {
            Request::Place(order) => (order.id, order.instrument, order.price.unwrap()),
            other => panic!("expected a place, got {other:?}"),
        }
    }

    /// A desired places at the instant it arrives; the ack a venue sends an
    /// instant later makes the slot working and sends nothing. The requests
    /// tick only on the instant that sent any, the pacing on every instant
    /// the node ran.
    #[test]
    fn a_desired_places_at_once_and_its_ack_makes_it_working() {
        let run = run(Script {
            desired: vec![(bid(A, "100", 1_000), 1_000)],
            reports: vec![(ack(id(1), 1_001), 1_001)],
            ..Script::default()
        });

        assert_eq!(run.requests.len(), 1, "{:?}", run.requests);
        let (at, burst) = &run.requests[0];
        assert_eq!(*at, t(1_000));
        assert_eq!(burst.len(), 1);
        assert_eq!(place(&burst[0]), (id(1), A, px("100")));

        assert_eq!(run.slot.len(), 2, "{:?}", run.slot);
        assert_eq!(run.slot[0].0, t(1_000));
        assert!(matches!(run.slot[0].1, Slot::Pending(_)), "{:?}", run.slot);
        assert_eq!(run.slot[1].0, t(1_001));
        assert!(matches!(run.slot[1].1, Slot::Working(_)), "{:?}", run.slot);

        let pacing: Vec<_> = run.pacing.iter().map(|(at, _)| *at).collect();
        assert_eq!(pacing, [t(1_000), t(1_001)]);
    }

    /// A cancel-all and a desired in one instant: the cancel-all goes at the
    /// head of the burst, and what the instant's decision wants placed
    /// follows it. The working order is left to the cancel-all.
    #[test]
    fn a_cancel_all_goes_ahead_of_the_same_instants_places() {
        let run = run(Script {
            desired: vec![(bid(A, "100", 1_000), 1_000), (bid(B, "50", 2_000), 2_000)],
            reports: vec![(ack(id(1), 1_001), 1_001)],
            cancel_all: vec![2_000],
            ..Script::default()
        });

        assert_eq!(run.requests.len(), 2, "{:?}", run.requests);
        let (at, burst) = &run.requests[1];
        assert_eq!(*at, t(2_000));
        assert_eq!(burst.len(), 2, "{burst:?}");
        assert_eq!(burst[0], Request::CancelAll);
        assert_eq!(place(&burst[1]), (id(2), B, px("50")));
        assert!(
            matches!(run.slot.last(), Some((_, Slot::PendingCancel(_)))),
            "{:?}",
            run.slot
        );
    }

    /// The sweep changes nothing and still drives the diff: past
    /// `max_desired_age`, with nothing new arriving, the working order is
    /// cancelled at the sweep's instant.
    #[test]
    fn a_sweep_alone_withdraws_a_stale_desired() {
        let fresh = 500_000_000;
        let stale = 1_000 + AGE.as_nanos() as u64 + 1;
        let run = run(Script {
            desired: vec![(bid(A, "100", 1_000), 1_000)],
            reports: vec![(ack(id(1), 1_001), 1_001)],
            sweep: vec![fresh, stale],
            ..Script::default()
        });

        let requests: Vec<_> = run
            .requests
            .iter()
            .map(|(at, burst)| (*at, burst.to_vec()))
            .collect();
        assert_eq!(requests.len(), 2, "{requests:?}");
        assert_eq!(requests[0].0, t(1_000));
        assert_eq!(requests[1], (t(stale), vec![Request::Cancel(id(1))]));
        // The first sweep ran the diff with the desired still fresh: a
        // cycle, a pacing, and nothing sent.
        let cycles: Vec<_> = run.pacing.iter().map(|(at, _)| *at).collect();
        assert_eq!(cycles, [t(1_000), t(1_001), t(fresh), t(stale)]);
    }

    /// A report and a desired in one instant: the report is applied first. A
    /// fill to nothing frees the slot, so the re-desire places a new order —
    /// applied the other way round it would have amended the filled one.
    #[test]
    fn a_report_is_applied_before_the_same_instants_desired() {
        let run = run(Script {
            desired: vec![(bid(A, "100", 1_000), 1_000), (bid(A, "101", 2_000), 2_000)],
            reports: vec![(ack(id(1), 1_001), 1_001), (filled(id(1), 2_000), 2_000)],
            ..Script::default()
        });

        assert_eq!(run.requests.len(), 2, "{:?}", run.requests);
        let (at, burst) = &run.requests[1];
        assert_eq!(*at, t(2_000));
        assert_eq!(burst.len(), 1, "{burst:?}");
        assert_eq!(place(&burst[0]), (id(2), A, px("101")));
    }

    /// The same script twice is the same run, instant for instant.
    #[test]
    fn a_run_is_deterministic() {
        let script = Script {
            desired: vec![
                (bid(A, "100", 1_000), 1_000),
                (bid(B, "50", 1_000), 1_000),
                (bid(A, "101", 2_000), 2_000),
            ],
            reports: vec![
                (ack(id(1), 1_001), 1_001),
                (ack(id(2), 1_001), 1_001),
                (filled(id(1), 1_500), 1_500),
                (
                    Report::Cancelled(Retired {
                        order: id(2),
                        recv_time: t(3_001),
                        ..Retired::default()
                    }),
                    3_001,
                ),
            ],
            cancel_all: vec![3_000],
            sweep: vec![2_500, 4_000],
        };
        let first = run(script.clone());
        assert!(first.requests.len() >= 3, "{:?}", first.requests);
        assert_eq!(first, run(script));
    }

    /// The short form with no trading state and no cancel-all is the long
    /// form handed streams that never tick: the same requests, pacing and
    /// slot, at the same instants — places, an amend, a stale withdrawal.
    #[test]
    fn the_short_form_is_the_long_form_with_inputs_that_never_tick() {
        let stale = 2_000 + AGE.as_nanos() as u64 + 1;
        let script = Script {
            desired: vec![
                (bid(A, "100", 1_000), 1_000),
                (bid(B, "50", 1_000), 1_000),
                (bid(A, "101", 2_000), 2_000),
            ],
            reports: vec![(ack(id(1), 1_001), 1_001), (ack(id(2), 1_001), 1_001)],
            sweep: vec![1_500, stale],
            ..Script::default()
        };
        let short = run_bare(script.clone(), false);
        let long = run_bare(script, true);

        // Both orders placed, A amended, then everything withdrawn as stale.
        let at: Vec<_> = short.requests.iter().map(|(at, _)| *at).collect();
        assert_eq!(at, [t(1_000), t(2_000), t(stale)], "{:?}", short.requests);
        assert_eq!(short, long);
    }
}
