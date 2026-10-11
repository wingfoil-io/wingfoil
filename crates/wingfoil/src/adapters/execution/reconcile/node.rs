//! Reconciliation as a graph node: [`ReconcileOp`], wired through
//! [`ReconcileOps`].
//!
//! The short form names the inputs a caller has and leaves out the rest:
//!
//! ```
//! use std::time::Duration;
//! use wingfoil::prelude::*;
//! use wingfoil::adapters::execution::edge::Holdings;
//! use wingfoil::adapters::execution::order::{Fill, Instrument};
//! use wingfoil::adapters::execution::position::Measure;
//! use wingfoil::adapters::execution::reconcile::node::Standing;
//! use wingfoil::adapters::execution::reconcile::{Config, Mismatch, ReconcileOps};
//! use wingfoil::adapters::market::Px;
//!
//! fn reconciled<I: Instrument + 'static>(
//!     fills: &Stream<Burst<Fill<I>>>,
//!     holdings: &Stream<Burst<Holdings<I>>>,
//!     marks: &Stream<Burst<(I, Px)>>,
//!     sweep: &Stream<()>,
//! ) -> (Stream<Burst<Mismatch<I>>>, Stream<Standing<I>>) {
//!     let config = Config { grace: Duration::from_secs(5) };
//!     fills
//!         .wire_reconcile(config, |_: &I| Some(Measure::Linear), holdings)
//!         .marks(marks)
//!         .sweep(sweep)
//!         .build()
//! }
//! ```
//!
//! `.settlements(..)`, `.cashflows(..)`, `.marks(..)` and `.reading(..)`
//! add the optional inputs and the caller's own look ([`ReconcileWiring`]);
//! an input left out is wired as a stream that never ticks. The sweep is the
//! one input that cannot be left out: without `.sweep(..)` there is no
//! `build`. [`ReconcileOps::reconcile`] takes every input positionally and
//! wires the same node.
//!
//! # One node, holding the book and the reconciler
//!
//! [`Reconciler::reconcile`] takes the fold it corrects as
//! `&mut impl Fold`: it reads the [`Book`] and re-bases it in place. The
//! reconciler's fold *is* the book. A reconciliation node beside a position
//! node would therefore need the book as state two nodes share — one
//! applying fills to it, the other re-basing it — which is a `RefCell` (or a
//! lock) on the graph path, and this codebase forbids both there. Shipping a
//! copy of the book down an edge instead would have the re-base correct the
//! copy, while the fold every other reader takes stayed wrong. So this node
//! holds both, and is a **superset of** [`PositionOp`]: the same three fold
//! steps, through the same function, so the two cannot drift, and then
//! reconciliation. A caller that reconciles wires this node *instead of* the
//! position node, not beside it — two books would be two folds of the same
//! fills, one re-based and one not.
//!
//! The two ops share the fold step and nothing else. Composing
//! [`PositionOp`] whole would not save anything: its state is the book
//! alone, its reading sees the book alone, and its output ticks a reading
//! where this one also has the instant's mismatches to carry.
//!
//! # The mark a re-base is booked at is an input
//!
//! A re-base books the difference at the fold's own mark at the instant it
//! happens ([`Book::rebase`]), and a mark moves. A closure in the `Cfg` could
//! only say a constant — a `Cfg` closure is `Fn`, and a compiled graph
//! re-creates it — or read a cell something else writes, which is the
//! `RefCell` again. So the marks are an edge, `(instrument, mark)` pairs,
//! and the node keeps the latest per instrument ([`Desk::mark`]). An instrument
//! with no mark yet is re-based without one: the net moves and the value is
//! dropped and counted, as [`Book::rebase`] says. Left out, every re-base is
//! that one.
//!
//! # The order within an instant
//!
//! 1. **fills** — [`Book::apply`], the whole burst in order. An ordinary fill
//!    is folded and nothing more: the venue's side moves with it, and its
//!    next snapshot says so;
//! 2. **settlements** — [`Book::apply`], and [`Reconciler::settled`]: the
//!    venue closed the position at its cut, so its side is flat too. This is
//!    the settlement edge, and only settlements reach it — a fill sent there
//!    would flatten the venue's stated holding on every trade;
//! 3. **cashflows** — [`Book::cashflow`], each in order;
//! 4. **holdings** — [`Reconciler::heard`], **each** snapshot in the burst in
//!    order, never only the last: a whole snapshot and the changes after it
//!    in one instant mean the snapshot and then the changes, and keeping the
//!    latest would drop what the snapshot said of everything the changes do
//!    not name;
//! 5. **marks** — the latest mark per instrument, for a re-base;
//! 6. **sweep** — nothing: it exists so the node runs with nothing new;
//!
//! then [`Reconciler::reconcile`] at the engine time, on **every** cycle the
//! node runs — the sweep's included, which is what judges the grace with
//! nothing new arriving: a difference outlives its grace on the clock, not
//! on the next message. The fold steps come first so a fill and the
//! snapshot that includes it, in one instant, agree.
//!
//! A venue's `Session::holdings` is one snapshot an instant; a caller lifts
//! it into a burst of one for this edge.
//!
//! A graph drives reconciliation through [`ReconcileOps`], and only through
//! it. The pure [`Reconciler`] stays for tests and for tools that are not
//! graphs.
//!
//! [`PositionOp`]: crate::adapters::execution::position::PositionOp

use std::collections::BTreeMap;
use std::marker::PhantomData;

use crate::op::{Activation, Ctx, Op, Tick};
use crate::prelude::*;
use anyhow::Result;

use super::{Config, Fold, Mismatch, Reconciler};
use crate::NanoTime;
use crate::adapters::execution::edge::{Cashflow, Holdings, non_empty};
use crate::adapters::execution::order::{Fill, Instrument};
use crate::adapters::execution::position::node::fold;
use crate::adapters::execution::position::{Book, Measure, Position};
use crate::adapters::market::{Px, Qty};

/// What [`ReconcileOp`] holds — the book, the latest marks, the reconciler
/// — and what this cycle's reconcile found: the node's state, and what its
/// `read` is handed after each reconcile. A look, never a lever: there is
/// nothing here to mutate it with.
pub struct Desk<I: Instrument> {
    book: Book<I>,
    marks: BTreeMap<I, Px>,
    reconciler: Reconciler<I>,
    found: Burst<Mismatch<I>>,
}

impl<I: Instrument> Desk<I> {
    /// The fold, after this cycle's inputs and any re-base.
    pub fn book(&self) -> &Book<I> {
        &self.book
    }

    /// The venue's side, and how long each difference has stood.
    pub fn reconciler(&self) -> &Reconciler<I> {
        &self.reconciler
    }

    /// The latest mark for `instrument` the `marks` input stated — what a
    /// re-base would be booked at — if it has stated one.
    pub fn mark(&self, instrument: &I) -> Option<Px> {
        self.marks.get(instrument).copied()
    }

    /// What this cycle's reconcile found, each now re-based on: the burst the
    /// node's mismatch stream carries, handed to `read` too so a reading can
    /// carry both in one value (a kill switch's, say).
    pub fn mismatches(&self) -> &[Mismatch<I>] {
        &self.found
    }
}

/// The node's fold, for one reconcile: the book, and the marks a re-base is
/// booked at. Borrowed, so the reconciler beside it can be borrowed too.
struct Marked<'a, I> {
    book: &'a mut Book<I>,
    marks: &'a BTreeMap<I, Px>,
}

impl<I: Instrument> Fold<I> for Marked<'_, I> {
    fn positions(&self) -> &Book<I> {
        self.book
    }

    fn rebase(&mut self, _now: NanoTime, instrument: I, net: Qty) {
        let mark = self.marks.get(&instrument).copied();
        self.book.rebase(instrument, net, mark);
    }
}

/// Reconciliation as an op: a [`Book`], the latest marks and a
/// [`Reconciler`] held as node state ([`Desk`]), fed their six inputs in the
/// fixed order the [module docs](self) state, emitting the instant's
/// mismatches and what `read` makes of the desk after the reconcile.
///
/// Wire it with [`ReconcileOps`]; the op itself is public so a compiled
/// graph can name it.
///
/// - `Cfg` — the reconciler's [`Config`], `measure`, the valuation
///   [`Book::new`] is handed, and `read`, a look at the [`Desk`] after the
///   reconcile.
/// - `State` — the [`Desk`], built on the first cycle.
/// - `Out` — `(mismatches, read)`, every cycle the node runs; the mismatch
///   burst may be empty.
pub struct ReconcileOp<I, M, R, F>(PhantomData<(I, M, R, F)>);

#[crate::op(build = reconcile_op)]
impl<I, M, R, F> Op for ReconcileOp<I, M, R, F>
where
    I: Instrument + 'static,
    M: Fn(&I) -> Option<Measure> + Clone + 'static,
    R: Clone + 'static,
    F: Fn(&Desk<I>) -> R + 'static,
{
    type Cfg = (Config, M, F);
    type State = Option<Desk<I>>;
    type In<'a> = (
        (&'a Burst<Fill<I>>, bool),
        (&'a Burst<Fill<I>>, bool),
        (&'a Burst<Cashflow<I>>, bool),
        (&'a Burst<Holdings<I>>, bool),
        (&'a Burst<(I, Px)>, bool),
        (&'a (), bool),
    );
    type Out = (Burst<Mismatch<I>>, R);
    const ACTIVATION: Activation = Activation::NONE;

    fn cycle(
        cfg: &mut (Config, M, F),
        state: &mut Option<Desk<I>>,
        input: Self::In<'_>,
        ctx: &mut Ctx<'_>,
    ) -> Result<Tick<(Burst<Mismatch<I>>, R)>> {
        let (config, measure, read) = cfg;
        let desk = state.get_or_insert_with(|| Desk {
            book: Book::new(measure.clone()),
            marks: BTreeMap::new(),
            reconciler: Reconciler::new(*config),
            found: Burst::new(),
        });
        // The sweep's flag is not read: it changes nothing, and the
        // reconcile below runs on every cycle whichever input woke the node.
        let (fills, settlements, cashflows, (holdings, stated), (marks, marked), _sweep) = input;
        fold(&mut desk.book, fills, settlements, cashflows);
        if let (settlements, true) = settlements {
            desk.reconciler.settled(settlements);
        }
        if stated {
            for snapshot in holdings {
                desk.reconciler.heard(snapshot);
            }
        }
        if marked {
            // A later mark for an instrument replaces an earlier.
            for (instrument, mark) in marks {
                desk.marks.insert(*instrument, *mark);
            }
        }
        let mut fold = Marked {
            book: &mut desk.book,
            marks: &desk.marks,
        };
        desk.found = desk
            .reconciler
            .reconcile(ctx.time(), &mut fold)
            .into_iter()
            .collect();
        let read = read(desk);
        Ok(Tick::Value((desk.found.clone(), read)))
    }
}

/// Where reconciliation stands after a cycle, with the fold it left: the
/// reading the short form emits when no other is named.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Standing<I> {
    /// Whether the venue has stated its holdings yet; nothing is judged
    /// before it has ([`Reconciler::has_heard`]).
    pub heard: bool,
    /// Positions the venue disagrees on and still inside their grace
    /// ([`Reconciler::disagreeing`]).
    pub disagreeing: usize,
    /// Positions re-based on the venue, ever ([`Reconciler::rebased`]).
    pub rebased: u64,
    /// The fold after the reconcile, in the instrument's order.
    pub positions: Vec<Position<I>>,
}

/// [`Standing`], read off the node: a fn item, so a compiled graph can name
/// it as a `Cfg`.
pub fn read_standing<I: Instrument>(desk: &Desk<I>) -> Standing<I> {
    let (book, reconciler) = (desk.book(), desk.reconciler());
    Standing {
        heard: reconciler.has_heard(),
        disagreeing: reconciler.disagreeing(),
        rebased: reconciler.rebased(),
        positions: book.positions(),
    }
}

/// Reconciliation, wired on a stream of fills. Out of any prelude:
/// `use wingfoil::adapters::execution::reconcile::ReconcileOps;`.
///
/// Start from [`wire_reconcile`](Self::wire_reconcile), which takes what
/// every reconciliation needs and returns a [`ReconcileWiring`] for the
/// rest:
///
/// ```ignore
/// let (mismatches, standing) = fills
///     .wire_reconcile(config, measure, &holdings)
///     .sweep(&clock)                      // required: no `build` without it
///     .settlements(&session.settlements)  // optional: a venue that delivers
///     .cashflows(&session.cashflows)      // optional
///     .marks(&marks)                      // optional: what a re-base is booked at
///     .build();
/// ```
///
/// Six inputs, applied within one instant in this order whichever of them
/// ticked:
///
/// 1. `self` — the fills on our orders, folded into the book;
/// 2. `settlements` — the venue's deliveries: folded, and the venue's side
///    flat in each. Left out, nothing settles;
/// 3. `cashflows` — booked on the book. Left out, none is;
/// 4. `holdings` — what the venue says it holds, every snapshot in order;
/// 5. `marks` — `(instrument, mark)`, the latest kept per instrument for a
///    re-base. Left out, a re-base moves the net and drops the value;
/// 6. `sweep` — a clock, changing nothing: the reconcile runs on it anyway,
///    which is what judges the grace with nothing new arriving. Never left
///    out: which clock that is is a choice.
///
/// Then the reconcile, at the engine time, on every cycle the node runs.
///
/// Returns the mismatches — each a difference that outlived the grace and
/// has been re-based on, for the caller to latch its risk on — ticking only
/// on a cycle that found any, and the reading on every cycle the node runs.
pub trait ReconcileOps<I: Instrument + 'static> {
    /// The book valued by `measure`, applying this stream's fills, read
    /// against `holdings` on `config`: the start of the short form. Name the
    /// sweep with [`sweep`](ReconcileWiring::sweep), add whichever optional
    /// inputs the graph has, then `build`.
    fn wire_reconcile<M>(
        &self,
        config: Config,
        measure: M,
        holdings: &Stream<Burst<Holdings<I>>>,
    ) -> ReconcileWiring<I, M>
    where
        M: Fn(&I) -> Option<Measure> + Clone + 'static;

    /// Reconciliation with every input named: see the trait docs for the
    /// inputs and the order they are applied in. A caller without some of
    /// them wants [`wire_reconcile`](Self::wire_reconcile).
    #[allow(clippy::too_many_arguments)]
    #[must_use = "a dropped stream stays wired and cycles every tick, producing an unread value"]
    fn reconcile<M, R, F>(
        &self,
        settlements: &Stream<Burst<Fill<I>>>,
        cashflows: &Stream<Burst<Cashflow<I>>>,
        holdings: &Stream<Burst<Holdings<I>>>,
        marks: &Stream<Burst<(I, Px)>>,
        sweep: &Stream<()>,
        config: Config,
        measure: M,
        read: F,
    ) -> (Stream<Burst<Mismatch<I>>>, Stream<R>)
    where
        M: Fn(&I) -> Option<Measure> + Clone + 'static,
        R: Clone + Default + 'static,
        F: Fn(&Desk<I>) -> R + 'static;
}

impl<I: Instrument + 'static> ReconcileOps<I> for Stream<Burst<Fill<I>>> {
    fn wire_reconcile<M>(
        &self,
        config: Config,
        measure: M,
        holdings: &Stream<Burst<Holdings<I>>>,
    ) -> ReconcileWiring<I, M>
    where
        M: Fn(&I) -> Option<Measure> + Clone + 'static,
    {
        ReconcileWiring {
            fills: self.clone(),
            config,
            measure,
            holdings: holdings.clone(),
            settlements: None,
            cashflows: None,
            marks: None,
            sweep: Unswept,
            read: (),
        }
    }

    fn reconcile<M, R, F>(
        &self,
        settlements: &Stream<Burst<Fill<I>>>,
        cashflows: &Stream<Burst<Cashflow<I>>>,
        holdings: &Stream<Burst<Holdings<I>>>,
        marks: &Stream<Burst<(I, Px)>>,
        sweep: &Stream<()>,
        config: Config,
        measure: M,
        read: F,
    ) -> (Stream<Burst<Mismatch<I>>>, Stream<R>)
    where
        M: Fn(&I) -> Option<Measure> + Clone + 'static,
        R: Clone + Default + 'static,
        F: Fn(&Desk<I>) -> R + 'static,
    {
        let node = wire(
            self,
            settlements,
            cashflows,
            holdings,
            marks,
            sweep,
            (config, measure, read),
        );
        split(&node)
    }
}

/// The reconciliation node's inputs, named, from
/// [`ReconcileOps::wire_reconcile`]: the short form of
/// [`ReconcileOps::reconcile`] for a caller that has only some of them.
///
/// - [`sweep`](Self::sweep) — **required**; `build` exists only once it is
///   named. It is what judges the grace with nothing new arriving, so its
///   clock is the caller's choice to make.
/// - [`settlements`](Self::settlements), [`cashflows`](Self::cashflows),
///   [`marks`](Self::marks) — optional; each left out never ticks.
/// - [`reading`](Self::reading) — optional; left out, `build` emits
///   [`Standing`].
///
/// An input left out is wired as a stream that never ticks, so what `build`
/// wires is the node [`ReconcileOps::reconcile`] wires when handed
/// never-ticking streams. `S` is [`Unswept`] until the sweep is named; `F`
/// is `()` until a reading is.
#[must_use = "a ReconcileWiring wires nothing until `build` is called"]
pub struct ReconcileWiring<I: Instrument + 'static, M, S = Unswept, F = ()> {
    fills: Stream<Burst<Fill<I>>>,
    config: Config,
    measure: M,
    holdings: Stream<Burst<Holdings<I>>>,
    settlements: Option<Stream<Burst<Fill<I>>>>,
    cashflows: Option<Stream<Burst<Cashflow<I>>>>,
    marks: Option<Stream<Burst<(I, Px)>>>,
    sweep: S,
    read: F,
}

/// A [`ReconcileWiring`] whose sweep is not named yet: it has no `build`.
#[derive(Clone, Copy, Debug)]
pub struct Unswept;

/// A [`ReconcileWiring`]'s look, from [`ReconcileWiring::reading`].
pub struct Reading<F>(F);

/// The optional edges, each one left out a stream that never ticks.
type Optional<I> = (
    Stream<Burst<Fill<I>>>,
    Stream<Burst<Cashflow<I>>>,
    Stream<Burst<(I, Px)>>,
);

impl<I: Instrument + 'static, M, S, F> ReconcileWiring<I, M, S, F> {
    /// The clock the reconcile runs on with nothing new arriving: what
    /// judges the grace. Required.
    pub fn sweep(self, sweep: &Stream<()>) -> ReconcileWiring<I, M, Stream<()>, F> {
        ReconcileWiring {
            fills: self.fills,
            config: self.config,
            measure: self.measure,
            holdings: self.holdings,
            settlements: self.settlements,
            cashflows: self.cashflows,
            marks: self.marks,
            sweep: sweep.clone(),
            read: self.read,
        }
    }

    /// The venue's settlements: folded after the instant's fills, and the
    /// venue's side flat in each — `session.settlements`. Only settlements:
    /// an ordinary fill here would flatten the venue's side on every trade.
    pub fn settlements(mut self, settlements: &Stream<Burst<Fill<I>>>) -> Self {
        self.settlements = Some(settlements.clone());
        self
    }

    /// The venue's cashflows, booked after the fills and settlements —
    /// `session.cashflows`.
    pub fn cashflows(mut self, cashflows: &Stream<Burst<Cashflow<I>>>) -> Self {
        self.cashflows = Some(cashflows.clone());
        self
    }

    /// `(instrument, mark)` pairs, the latest kept per instrument: what a
    /// re-base is booked at.
    pub fn marks(mut self, marks: &Stream<Burst<(I, Px)>>) -> Self {
        self.marks = Some(marks.clone());
        self
    }

    fn optional(&self) -> Optional<I> {
        let graph = self.fills.graph();
        let settlements = match &self.settlements {
            Some(settlements) => settlements.clone(),
            None => graph.never().map(|(): &()| Burst::new()),
        };
        let cashflows = match &self.cashflows {
            Some(cashflows) => cashflows.clone(),
            None => graph.never().map(|(): &()| Burst::new()),
        };
        let marks = match &self.marks {
            Some(marks) => marks.clone(),
            None => graph.never().map(|(): &()| Burst::new()),
        };
        (settlements, cashflows, marks)
    }
}

impl<I: Instrument + 'static, M, S> ReconcileWiring<I, M, S, ()> {
    /// The caller's own look at the [`Desk`] after each cycle's reconcile,
    /// emitted by `build` in place of [`Standing`] —
    /// [`ReconcileOps::reconcile`]'s `read`. A look, never a lever.
    pub fn reading<R, F>(self, read: F) -> ReconcileWiring<I, M, S, Reading<F>>
    where
        R: Clone + Default + 'static,
        F: Fn(&Desk<I>) -> R + 'static,
    {
        ReconcileWiring {
            fills: self.fills,
            config: self.config,
            measure: self.measure,
            holdings: self.holdings,
            settlements: self.settlements,
            cashflows: self.cashflows,
            marks: self.marks,
            sweep: self.sweep,
            read: Reading(read),
        }
    }
}

impl<I, M> ReconcileWiring<I, M, Stream<()>, ()>
where
    I: Instrument + 'static,
    M: Fn(&I) -> Option<Measure> + Clone + 'static,
{
    /// Wire the node: the mismatches, ticking only on a cycle that found
    /// any, and the [`Standing`] on every cycle the node runs.
    #[must_use = "a dropped stream stays wired and cycles every tick, producing an unread value"]
    pub fn build(self) -> (Stream<Burst<Mismatch<I>>>, Stream<Standing<I>>) {
        let (settlements, cashflows, marks) = self.optional();
        let node = wire(
            &self.fills,
            &settlements,
            &cashflows,
            &self.holdings,
            &marks,
            &self.sweep,
            (self.config, self.measure, read_standing::<I>),
        );
        split(&node)
    }
}

impl<I, M, R, F> ReconcileWiring<I, M, Stream<()>, Reading<F>>
where
    I: Instrument + 'static,
    M: Fn(&I) -> Option<Measure> + Clone + 'static,
    R: Clone + Default + 'static,
    F: Fn(&Desk<I>) -> R + 'static,
{
    /// Wire the node: the mismatches, and what the
    /// [`reading`](ReconcileWiring::reading) makes of it every cycle the
    /// node runs — as [`ReconcileOps::reconcile`].
    #[must_use = "a dropped stream stays wired and cycles every tick, producing an unread value"]
    pub fn build(self) -> (Stream<Burst<Mismatch<I>>>, Stream<R>) {
        let (settlements, cashflows, marks) = self.optional();
        let node = wire(
            &self.fills,
            &settlements,
            &cashflows,
            &self.holdings,
            &marks,
            &self.sweep,
            (self.config, self.measure, self.read.0),
        );
        split(&node)
    }
}

/// The node itself, its edges in the order [`ReconcileOp`] reads them.
fn wire<I, M, R, F>(
    fills: &Stream<Burst<Fill<I>>>,
    settlements: &Stream<Burst<Fill<I>>>,
    cashflows: &Stream<Burst<Cashflow<I>>>,
    holdings: &Stream<Burst<Holdings<I>>>,
    marks: &Stream<Burst<(I, Px)>>,
    sweep: &Stream<()>,
    cfg: (Config, M, F),
) -> Stream<(Burst<Mismatch<I>>, R)>
where
    I: Instrument + 'static,
    M: Fn(&I) -> Option<Measure> + Clone + 'static,
    R: Clone + Default + 'static,
    F: Fn(&Desk<I>) -> R + 'static,
{
    let (settlements, cashflows, holdings, marks, sweep) = (
        settlements.handle(),
        cashflows.handle(),
        holdings.handle(),
        marks.handle(),
        sweep.handle(),
    );
    fills.wire(move |b, fills| {
        b.reconcile_op(fills, settlements, cashflows, holdings, marks, sweep, cfg)
    })
}

/// The mismatches, on a cycle that found any, and the reading, every cycle.
fn split<I, R>(node: &Stream<(Burst<Mismatch<I>>, R)>) -> (Stream<Burst<Mismatch<I>>>, Stream<R>)
where
    I: Instrument + 'static,
    R: Clone + Default + 'static,
{
    let mismatches = non_empty(node, |(mismatches, _)| mismatches);
    let read = node.map(|(_, read): &(Burst<Mismatch<I>>, R)| read.clone());
    (mismatches, read)
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use crate::adapters::market::Side;
    use crate::{RunFor, RunMode};

    use super::*;
    use crate::adapters::execution::edge::Held;
    use crate::adapters::execution::exec_id::ExecId;
    use crate::adapters::execution::order::{ClientOrderId, Liquidity};

    #[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash, PartialOrd, Ord)]
    struct Ticker(u8);

    const A: Ticker = Ticker(1);
    const B: Ticker = Ticker(2);

    const SEC: u64 = 1_000_000_000;
    const GRACE: Duration = Duration::from_secs(3);

    fn config() -> Config {
        Config { grace: GRACE }
    }

    fn linear(_: &Ticker) -> Option<Measure> {
        Some(Measure::Linear)
    }

    fn t(secs: u64) -> NanoTime {
        NanoTime::new(secs * SEC)
    }

    fn q(text: &str) -> Qty {
        Qty::parse(text).unwrap()
    }

    fn px(text: &str) -> Px {
        Px::parse(text).unwrap()
    }

    fn fill(ticker: Ticker, side: Side, qty: &str, price: &str) -> Fill<Ticker> {
        Fill {
            order: ClientOrderId(1),
            exec_id: ExecId::new("e").unwrap(),
            instrument: ticker,
            side,
            qty: q(qty),
            filled: q(qty),
            remaining: Qty::ZERO,
            price: px(price),
            liquidity: Liquidity::Maker,
            ..Default::default()
        }
    }

    fn holdings(whole: bool, held: &[(Ticker, &str)]) -> Holdings<Ticker> {
        Holdings {
            whole,
            held: held
                .iter()
                .map(|(instrument, net)| Held {
                    instrument: *instrument,
                    net: q(net),
                })
                .collect(),
            as_of: NanoTime::ZERO,
        }
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

    /// What one run is fed, each row at its engine second.
    #[derive(Clone, Default)]
    struct Script {
        fills: Vec<(Fill<Ticker>, u64)>,
        settlements: Vec<(Fill<Ticker>, u64)>,
        holdings: Vec<(Holdings<Ticker>, u64)>,
        marks: Vec<((Ticker, Px), u64)>,
        sweep: Vec<u64>,
    }

    /// `(heard, disagreeing, rebased, A's net, B's net)`.
    type Line = (bool, usize, u64, Qty, Qty);

    /// What one run emitted, each with the instant it ticked at.
    #[derive(Debug, PartialEq)]
    struct Run {
        mismatches: Vec<(NanoTime, Vec<Mismatch<Ticker>>)>,
        standing: Vec<(NanoTime, Line)>,
        /// B's entry after each cycle: where a re-base booked it.
        entry: Vec<(NanoTime, Option<Px>)>,
    }

    fn run(script: Script) -> Run {
        let g = GraphBuilder::new();
        let fills = rows(&g, script.fills);
        let settlements = rows(&g, script.settlements);
        let holdings = rows(&g, script.holdings);
        let marks = rows(&g, script.marks);
        let sweep = beats(&g, script.sweep);

        let (mismatches, standing) = fills
            .wire_reconcile(config(), linear, &holdings)
            .settlements(&settlements)
            .marks(&marks)
            .sweep(&sweep)
            .build();
        let entry = standing.map(|s: &Standing<Ticker>| {
            s.positions
                .iter()
                .find(|p| p.instrument == B)
                .and_then(|p| p.entry())
        });
        let mismatches = mismatches.with_time().accumulate();
        let standing = standing.with_time().accumulate();
        let entry = entry.with_time().accumulate();
        let mut runner = g.build();
        runner
            .run(RunMode::HistoricalFrom(NanoTime::ZERO), RunFor::Forever)
            .unwrap();

        let net = |s: &Standing<Ticker>, ticker| {
            s.positions
                .iter()
                .find(|p| p.instrument == ticker)
                .map_or(Qty::ZERO, |p| p.net())
        };
        Run {
            mismatches: runner
                .value(&mismatches)
                .iter()
                .map(|(at, burst)| (*at, burst.to_vec()))
                .collect(),
            standing: runner
                .value(&standing)
                .iter()
                .map(|(at, s)| {
                    let line = (s.heard, s.disagreeing, s.rebased, net(s, A), net(s, B));
                    (*at, line)
                })
                .collect(),
            entry: runner.value(&entry),
        }
    }

    /// A fill and the snapshot that already includes it, in one instant:
    /// the fill is folded first, so the two agree and no grace starts.
    #[test]
    fn a_fill_is_folded_before_the_same_instants_holdings() {
        let run = run(Script {
            fills: vec![(fill(A, Side::Bid, "5", "100"), 1)],
            holdings: vec![(holdings(true, &[(A, "5")]), 1)],
            sweep: vec![10],
            ..Script::default()
        });
        assert_eq!(run.mismatches, vec![]);
        assert_eq!(
            run.standing,
            vec![
                (t(1), (true, 0, 0, q("5"), Qty::ZERO)),
                (t(10), (true, 0, 0, q("5"), Qty::ZERO)),
            ]
        );
    }

    /// The venue states a position the fold never saw. Nothing new arrives
    /// after it; the sweep alone judges the grace, and the mismatch is found
    /// at the first sweep past it — re-based at the latest mark — ticking
    /// only then. Before the venue speaks, nothing is judged.
    #[test]
    fn the_sweep_judges_the_grace_with_nothing_new_arriving() {
        let run = run(Script {
            fills: vec![(fill(A, Side::Bid, "1", "100"), 0)],
            holdings: vec![(holdings(true, &[(A, "1"), (B, "30")]), 1)],
            marks: vec![((B, px("40")), 1), ((B, px("42")), 3)],
            sweep: vec![2, 3, 4, 5],
            ..Script::default()
        });
        let (zero, one) = (Qty::ZERO, q("1"));
        assert_eq!(
            run.standing,
            vec![
                (t(0), (false, 0, 0, one, zero)),
                (t(1), (true, 1, 0, one, zero)),
                (t(2), (true, 1, 0, one, zero)),
                (t(3), (true, 1, 0, one, zero)),
                (t(4), (true, 0, 1, one, q("30"))),
                (t(5), (true, 0, 1, one, q("30"))),
            ]
        );
        assert_eq!(
            run.mismatches,
            vec![(
                t(4),
                vec![Mismatch {
                    instrument: B,
                    ours: zero,
                    venue: q("30"),
                    since: t(1),
                }]
            )]
        );
        assert_eq!(run.entry.last(), Some(&(t(5), Some(px("42")))));
    }

    /// A whole snapshot and a change after it, in one burst: both are heard,
    /// in order. Keeping only the last would lose what the snapshot said of
    /// A, and the fold — which holds both — would disagree on it.
    #[test]
    fn every_snapshot_in_a_burst_is_heard_in_order() {
        let run = run(Script {
            fills: vec![
                (fill(A, Side::Bid, "5", "100"), 0),
                (fill(B, Side::Bid, "2", "100"), 0),
            ],
            holdings: vec![
                (holdings(true, &[(A, "5")]), 1),
                (holdings(false, &[(B, "2")]), 1),
            ],
            sweep: vec![10],
            ..Script::default()
        });
        assert_eq!(run.mismatches, vec![]);
        assert_eq!(
            run.standing.last(),
            Some(&(t(10), (true, 0, 0, q("5"), q("2"))))
        );
    }

    /// A settlement flattens the venue's side as well as the fold's, so a
    /// delivery the venue does not restate agrees. An ordinary fill does not:
    /// the venue's side waits for its next snapshot, and until then the two
    /// disagree inside the grace.
    #[test]
    fn a_settlement_flattens_the_venue_and_a_fill_does_not() {
        let run = run(Script {
            fills: vec![
                (fill(A, Side::Bid, "1", "100"), 0),
                (fill(B, Side::Bid, "1", "100"), 0),
                (fill(B, Side::Ask, "1", "101"), 2),
            ],
            settlements: vec![(fill(A, Side::Ask, "1", "105"), 2)],
            holdings: vec![(holdings(true, &[(A, "1"), (B, "1")]), 1)],
            ..Script::default()
        });
        let zero = Qty::ZERO;
        assert_eq!(
            run.standing,
            vec![
                (t(0), (false, 0, 0, q("1"), q("1"))),
                (t(1), (true, 0, 0, q("1"), q("1"))),
                (t(2), (true, 1, 0, zero, zero)),
            ]
        );
    }

    /// A reading of the caller's own is handed the desk after the
    /// reconcile: the instant's mismatches — the same burst the mismatch
    /// stream carries — and the latest mark, every cycle the node runs.
    #[test]
    fn a_reading_sees_the_instants_mismatches_and_the_latest_mark() {
        let g = GraphBuilder::new();
        let fills = rows(&g, vec![(fill(A, Side::Bid, "1", "100"), 0)]);
        let holdings = rows(&g, vec![(holdings(true, &[(B, "30")]), 1)]);
        let marks = rows(&g, vec![((B, px("40")), 2)]);
        let sweep = beats(&g, vec![5]);
        let (mismatches, read) = fills
            .wire_reconcile(config(), linear, &holdings)
            .marks(&marks)
            .sweep(&sweep)
            .reading(|desk: &Desk<Ticker>| (desk.mismatches().to_vec(), desk.mark(&B)))
            .build();
        let mismatches = mismatches.with_time().accumulate();
        let read = read.with_time().accumulate();
        let mut runner = g.build();
        runner
            .run(RunMode::HistoricalFrom(NanoTime::ZERO), RunFor::Forever)
            .unwrap();

        // The fold holds A, which the venue's snapshot calls flat, and not
        // B, which it holds: two differences, both from t=1.
        let found = vec![
            Mismatch {
                instrument: A,
                ours: q("1"),
                venue: Qty::ZERO,
                since: t(1),
            },
            Mismatch {
                instrument: B,
                ours: Qty::ZERO,
                venue: q("30"),
                since: t(1),
            },
        ];
        assert_eq!(
            runner.value(&read),
            vec![
                (t(0), (vec![], None)),
                (t(1), (vec![], None)),
                (t(2), (vec![], Some(px("40")))),
                (t(5), (found.clone(), Some(px("40")))),
            ]
        );
        let mismatches: Vec<_> = runner
            .value(&mismatches)
            .iter()
            .map(|(at, burst)| (*at, burst.to_vec()))
            .collect();
        assert_eq!(mismatches, vec![(t(5), found)]);
    }
}
