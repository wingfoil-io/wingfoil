//! The position fold as a graph node: [`PositionOp`], wired through
//! [`PositionOps`].
//!
//! The short form names the inputs a caller has and leaves out the rest:
//!
//! ```
//! use wingfoil::prelude::*;
//! use wingfoil::trading::execution::order::{Fill, Instrument};
//! use wingfoil::trading::execution::position::{Measure, Position, PositionOps};
//!
//! fn held<I: Instrument + 'static>(fills: &Stream<Burst<Fill<I>>>) -> Stream<Vec<Position<I>>> {
//!     fills.wire_position(|_: &I| Some(Measure::Linear)).build()
//! }
//! ```
//!
//! `.settlements(..)`, `.cashflows(..)` and `.reading(..)` add the optional
//! inputs and the caller's own look at the book ([`PositionWiring`]); an
//! input left out is wired as a stream that never ticks. Without a reading,
//! `build` emits [`read_positions`]: every position, in the instrument's
//! order. [`PositionOps::position`] takes every input positionally and
//! wires the same node.
//!
//! [`Book`] has two mutating entry points — [`apply`](Book::apply) and
//! [`cashflow`](Book::cashflow) — fed from three edges, and the order they
//! are called in within one instant is the node's, stated once:
//!
//! 1. **fills** — [`Book::apply`], the whole burst in order: the executions
//!    the venue reported on our orders;
//! 2. **settlements** — [`Book::apply`] too, the whole burst in order. A
//!    settlement answers no order — the venue closed the position at its cut
//!    — but the fold applies it as it applies any fill (the execution
//!    module's CLAUDE.md, "A settlement is not a `Report`"). After the
//!    fills, because a delivery closes what the instant's trading left;
//! 3. **cashflows** — [`Book::cashflow`], each in order: money moved on a
//!    position with no execution behind it, which moves no net, so it lands
//!    on the position the trades left;
//!
//! then `read`, a look at the book once all three are in, emitted on
//! **every** cycle the node runs.
//!
//! # Why a reading and not the book
//!
//! Every stream type needs a `Default` — the value slot is seeded with one —
//! and [`Book`] deliberately has none: a book that values nothing must be
//! asked for by name. So the node emits what `read` makes of the book, an
//! `R: Clone + Default`, exactly as [`OmsOps::oms_reading`] does for the
//! OMS: a look, never a lever. [`read_positions`] is the common case;
//! [`total`](super::total) sums positions carried off the book the way
//! [`Book::total`] sums them on it.
//!
//! A graph drives the position fold through [`PositionOps`], and only
//! through it — never a `Book` in a `RefCell` updated from a closure, and
//! never a `fold`, which has one input where the book has three. Where the
//! fold is also reconciled against the venue, wire
//! [`ReconcileOps`](crate::trading::execution::reconcile::ReconcileOps)
//! instead: its node holds the book too, and runs the same three steps first.
//!
//! [`OmsOps::oms_reading`]: crate::trading::execution::oms::OmsOps::oms_reading

use std::marker::PhantomData;

use crate::op::{Activation, Ctx, Op, Tick};
use crate::prelude::*;
use anyhow::Result;

use super::{Book, Measure, Position};
use crate::trading::execution::edge::{Cashflow, filler};
use crate::trading::execution::order::{Fill, Instrument};

/// The position fold as an op: a [`Book`] held as node state, fed its three
/// inputs in the fixed order the [module docs](self) state, emitting what
/// `read` makes of the book once they are in.
///
/// Wire it with [`PositionOps`]; the op itself is public so a compiled graph
/// can name it.
///
/// - `Cfg` — `measure`, the valuation [`Book::new`] is handed, and `read`, a
///   look at the book after the instant's inputs.
/// - `State` — the [`Book`], built from `measure` on the first cycle.
/// - `Out` — what `read` made of it, every cycle the node runs.
pub struct PositionOp<I, M, R, F>(PhantomData<(I, M, R, F)>);

#[crate::op(build = position_op, alias = position)]
impl<I, M, R, F> Op for PositionOp<I, M, R, F>
where
    I: Instrument + 'static,
    M: Fn(&I) -> Option<Measure> + Clone + 'static,
    R: Clone + 'static,
    F: Fn(&Book<I>) -> R + 'static,
{
    type Cfg = (M, F);
    type State = Option<Book<I>>;
    type In<'a> = (
        (&'a Burst<Fill<I>>, bool),
        (&'a Burst<Fill<I>>, bool),
        (&'a Burst<Cashflow<I>>, bool),
    );
    type Out = R;
    const ACTIVATION: Activation = Activation::NONE;

    fn cycle(
        cfg: &mut (M, F),
        state: &mut Option<Book<I>>,
        input: Self::In<'_>,
        _ctx: &mut Ctx<'_>,
    ) -> Result<Tick<R>> {
        let (measure, read) = cfg;
        let book = state.get_or_insert_with(|| Book::new(measure.clone()));
        let (fills, settlements, cashflows) = input;
        fold(book, fills, settlements, cashflows);
        Ok(Tick::Value(read(book)))
    }
}

/// The fold step [`PositionOp`] runs, and the one the reconciliation node
/// runs before it reconciles: fills, then settlements, then cashflows, each
/// only on a cycle its edge ticked.
pub(crate) fn fold<I: Instrument>(
    book: &mut Book<I>,
    (fills, filled): (&Burst<Fill<I>>, bool),
    (settlements, settled): (&Burst<Fill<I>>, bool),
    (cashflows, paid): (&Burst<Cashflow<I>>, bool),
) {
    if filled {
        book.apply(fills);
    }
    if settled {
        book.apply(settlements);
    }
    if paid {
        for cashflow in cashflows {
            book.cashflow(cashflow.instrument, cashflow.amount);
        }
    }
}

/// Every position in the book, in the instrument's order: the reading the
/// short form emits when no other is named, and a fn item, so a compiled
/// graph can name it as a `Cfg`.
pub fn read_positions<I: Instrument>(book: &Book<I>) -> Vec<Position<I>> {
    book.positions()
}

/// The position fold, wired on a stream of fills. Out of any prelude:
/// `use wingfoil::trading::execution::position::PositionOps;`.
///
/// Start from [`wire_position`](Self::wire_position), which takes what every
/// book needs and returns a [`PositionWiring`] for the rest:
///
/// ```ignore
/// let positions = fills
///     .wire_position(measure)
///     .settlements(&session.settlements)  // optional: a venue that delivers
///     .cashflows(&session.cashflows)      // optional: funding, coupons
///     .build();                           // Stream<Vec<Position<I>>>
/// ```
///
/// Three inputs, applied within one instant in this order whichever of them
/// ticked:
///
/// 1. `self` — the fills on our orders, applied in order;
/// 2. `settlements` — the venue's deliveries and expiries, applied in order
///    after the fills. Left out, nothing settles;
/// 3. `cashflows` — money moved on a position with nothing traded, booked in
///    order. Left out, none is.
///
/// Then the reading, every cycle the node runs.
pub trait PositionOps<I: Instrument + 'static> {
    /// The book valued by `measure` — `None` for an instrument the caller
    /// cannot value, whose net is still folded — applying this stream's
    /// fills: the start of the short form. Add whichever optional inputs the
    /// graph has, then `build`.
    fn wire_position<M>(&self, measure: M) -> PositionWiring<I, M>
    where
        M: Fn(&I) -> Option<Measure> + Clone + 'static;

    /// The book valued by `measure`, every input named, emitting what `read`
    /// makes of it every cycle the node runs: see the trait docs for the
    /// inputs and the order they are applied in. A caller without
    /// settlements or cashflows wants [`wire_position`](Self::wire_position).
    #[must_use = "a dropped stream stays wired and cycles every tick, producing an unread value"]
    fn position<M, R, F>(
        &self,
        settlements: &Stream<Burst<Fill<I>>>,
        cashflows: &Stream<Burst<Cashflow<I>>>,
        measure: M,
        read: F,
    ) -> Stream<R>
    where
        M: Fn(&I) -> Option<Measure> + Clone + 'static,
        R: Clone + Default + 'static,
        F: Fn(&Book<I>) -> R + 'static;
}

impl<I: Instrument + 'static> PositionOps<I> for Stream<Burst<Fill<I>>> {
    fn wire_position<M>(&self, measure: M) -> PositionWiring<I, M>
    where
        M: Fn(&I) -> Option<Measure> + Clone + 'static,
    {
        PositionWiring {
            fills: self.clone(),
            measure,
            settlements: None,
            cashflows: None,
            read: (),
        }
    }

    fn position<M, R, F>(
        &self,
        settlements: &Stream<Burst<Fill<I>>>,
        cashflows: &Stream<Burst<Cashflow<I>>>,
        measure: M,
        read: F,
    ) -> Stream<R>
    where
        M: Fn(&I) -> Option<Measure> + Clone + 'static,
        R: Clone + Default + 'static,
        F: Fn(&Book<I>) -> R + 'static,
    {
        wire(self, settlements, cashflows, measure, read)
    }
}

/// The position node's inputs, named, from [`PositionOps::wire_position`]:
/// the short form of [`PositionOps::position`] for a caller that has only
/// some of them.
///
/// - [`settlements`](Self::settlements) — optional; left out, nothing
///   settles.
/// - [`cashflows`](Self::cashflows) — optional; left out, none is booked.
/// - [`reading`](Self::reading) — optional; left out, `build` emits
///   [`read_positions`].
///
/// An input left out is wired as a stream that never ticks, so what `build`
/// wires is the node [`PositionOps::position`] wires when handed
/// never-ticking streams — same inputs, same order, same readings at the
/// same instants. `F` is `()` until a reading is named.
#[must_use = "a PositionWiring wires nothing until `build` is called"]
pub struct PositionWiring<I: Instrument + 'static, M, F = ()> {
    fills: Stream<Burst<Fill<I>>>,
    measure: M,
    settlements: Option<Stream<Burst<Fill<I>>>>,
    cashflows: Option<Stream<Burst<Cashflow<I>>>>,
    read: F,
}

/// A [`PositionWiring`]'s look at the book, from [`PositionWiring::reading`].
pub struct Reading<F>(F);

impl<I: Instrument + 'static, M, F> PositionWiring<I, M, F> {
    /// The venue's settlements — a delivery, an expiry — applied after the
    /// instant's fills: `session.settlements`.
    pub fn settlements(mut self, settlements: &Stream<Burst<Fill<I>>>) -> Self {
        self.settlements = Some(settlements.clone());
        self
    }

    /// The venue's cashflows — funding, a coupon, a dividend — booked after
    /// the instant's fills and settlements: `session.cashflows`.
    pub fn cashflows(mut self, cashflows: &Stream<Burst<Cashflow<I>>>) -> Self {
        self.cashflows = Some(cashflows.clone());
        self
    }

    /// The optional edges, each one left out a stream that never ticks.
    fn optional(&self) -> (Stream<Burst<Fill<I>>>, Stream<Burst<Cashflow<I>>>) {
        let graph = self.fills.graph();
        let settlements = match &self.settlements {
            Some(settlements) => settlements.clone(),
            None => graph.never().map(filler),
        };
        let cashflows = match &self.cashflows {
            Some(cashflows) => cashflows.clone(),
            None => graph.never().map(filler),
        };
        (settlements, cashflows)
    }
}

impl<I: Instrument + 'static, M> PositionWiring<I, M, ()> {
    /// The caller's own look at the book, emitted by `build` in place of
    /// [`read_positions`] — [`PositionOps::position`]'s `read`. A look,
    /// never a lever: `read` has the book by shared reference.
    pub fn reading<R, F>(self, read: F) -> PositionWiring<I, M, Reading<F>>
    where
        R: Clone + Default + 'static,
        F: Fn(&Book<I>) -> R + 'static,
    {
        PositionWiring {
            fills: self.fills,
            measure: self.measure,
            settlements: self.settlements,
            cashflows: self.cashflows,
            read: Reading(read),
        }
    }
}

impl<I, M> PositionWiring<I, M, ()>
where
    I: Instrument + 'static,
    M: Fn(&I) -> Option<Measure> + Clone + 'static,
{
    /// Wire the node: every position, in the instrument's order, on every
    /// cycle the node runs — [`read_positions`].
    #[must_use = "a dropped stream stays wired and cycles every tick, producing an unread value"]
    pub fn build(self) -> Stream<Vec<Position<I>>> {
        let (settlements, cashflows) = self.optional();
        wire(
            &self.fills,
            &settlements,
            &cashflows,
            self.measure,
            read_positions::<I>,
        )
    }
}

impl<I, M, R, F> PositionWiring<I, M, Reading<F>>
where
    I: Instrument + 'static,
    M: Fn(&I) -> Option<Measure> + Clone + 'static,
    R: Clone + Default + 'static,
    F: Fn(&Book<I>) -> R + 'static,
{
    /// Wire the node: what the [`reading`](PositionWiring::reading) makes of
    /// the book, on every cycle the node runs — as [`PositionOps::position`].
    #[must_use = "a dropped stream stays wired and cycles every tick, producing an unread value"]
    pub fn build(self) -> Stream<R> {
        let (settlements, cashflows) = self.optional();
        wire(
            &self.fills,
            &settlements,
            &cashflows,
            self.measure,
            self.read.0,
        )
    }
}

/// The node itself, its edges in the order [`PositionOp`] reads them.
fn wire<I, M, R, F>(
    fills: &Stream<Burst<Fill<I>>>,
    settlements: &Stream<Burst<Fill<I>>>,
    cashflows: &Stream<Burst<Cashflow<I>>>,
    measure: M,
    read: F,
) -> Stream<R>
where
    I: Instrument + 'static,
    M: Fn(&I) -> Option<Measure> + Clone + 'static,
    R: Clone + Default + 'static,
    F: Fn(&Book<I>) -> R + 'static,
{
    let (settlements, cashflows) = (settlements.handle(), cashflows.handle());
    fills.wire(move |b, fills| b.position_op(fills, settlements, cashflows, (measure, read)))
}

// `PositionOps::wire_position` as a builder face: what a
// `.wire_position(..)…build()` chain means to a compiled graph — each option
// left out wired as `PositionWiring::optional` wires it, then the one
// `position` node. Keep the two together.
#[doc(hidden)]
#[macro_export]
macro_rules! __wf_composite_wire_position {
    ($($cb:tt)*) => {
        $crate::composite! { [$($cb)*]
            builder wire_position<I>(fills: &Stream<Burst<Fill<I>>>, measure: M) {
                settlements(settlements: &Stream<Burst<Fill<I>>>) =
                    |g| g.never().map($crate::trading::execution::edge::filler);
                cashflows(cashflows: &Stream<Burst<Cashflow<I>>>) =
                    |g| g.never().map($crate::trading::execution::edge::filler);
                reading(read: F);
                build() => fills.position(
                    settlements,
                    cashflows,
                    measure,
                    $crate::trading::execution::position::node::read_positions,
                );
                build(reading) => fills.position(settlements, cashflows, measure, read);
            }
        }
    };
}
#[doc(hidden)]
pub use crate::__wf_composite_wire_position;
__wf_composite_wire_position!(define);

#[cfg(test)]
mod tests {
    use crate::trading::market::{Px, Qty, Side};
    use crate::{NanoTime, RunFor, RunMode};

    use super::*;
    use crate::trading::execution::exec_id::ExecId;
    use crate::trading::execution::order::{ClientOrderId, Liquidity};

    /// A venue listing tickers by number; ticker 3 is one nothing values.
    #[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash, PartialOrd, Ord)]
    struct Ticker(u8);

    const A: Ticker = Ticker(1);
    const B: Ticker = Ticker(2);
    const SWAP: Ticker = Ticker(3);

    fn measure(ticker: &Ticker) -> Option<Measure> {
        (*ticker != SWAP).then_some(Measure::Linear)
    }

    fn t(nanos: u64) -> NanoTime {
        NanoTime::new(nanos)
    }

    fn q(text: &str) -> Qty {
        Qty::parse(text).unwrap()
    }

    fn fill(ticker: Ticker, side: Side, qty: &str, price: &str, fee: &str) -> Fill<Ticker> {
        Fill {
            order: ClientOrderId(1),
            exec_id: ExecId::new("e").unwrap(),
            instrument: ticker,
            side,
            qty: q(qty),
            filled: q(qty),
            remaining: Qty::ZERO,
            price: Px::parse(price).unwrap(),
            fee: q(fee),
            liquidity: Liquidity::Maker,
            ..Default::default()
        }
    }

    fn cashflow(ticker: Ticker, amount: &str) -> Cashflow<Ticker> {
        Cashflow {
            instrument: ticker,
            amount: q(amount),
            ..Cashflow::default()
        }
    }

    fn rows<T: Clone + Default + 'static>(
        g: &GraphBuilder,
        rows: Vec<(T, u64)>,
    ) -> Stream<Burst<T>> {
        g.replay(rows.into_iter().map(|(row, at)| (row, t(at))))
    }

    /// `(instrument, net, realised, fees, cashflows)` per position.
    type Line = (Ticker, Qty, Qty, Qty, Qty);

    fn lines(positions: &[Position<Ticker>]) -> Vec<Line> {
        positions
            .iter()
            .map(|p| (p.instrument, p.net(), p.realised(), p.fees(), p.cashflows()))
            .collect()
    }

    fn run(stream: Stream<Vec<Position<Ticker>>>, g: &GraphBuilder) -> Vec<(NanoTime, Vec<Line>)> {
        let out = stream.with_time().accumulate();
        let mut runner = g.build();
        runner
            .run(RunMode::HistoricalFrom(NanoTime::ZERO), RunFor::Forever)
            .unwrap();
        runner
            .value(&out)
            .iter()
            .map(|(at, positions)| (*at, lines(positions)))
            .collect()
    }

    /// A fill, the settlement that closes it and a cashflow, all in one
    /// instant: one cycle, one reading, with all three in it — the fill
    /// opened, the settlement closed at the cut and booked the difference,
    /// the cashflow landed on the position they left. The next instant's
    /// fill reads against that.
    #[test]
    fn fills_then_settlements_then_cashflows_land_in_one_reading() {
        let g = GraphBuilder::new();
        let fills = rows(
            &g,
            vec![
                (fill(A, Side::Bid, "2", "100", "1"), 1_000),
                (fill(B, Side::Ask, "1", "50", "0"), 2_000),
            ],
        );
        let settlements = rows(&g, vec![(fill(A, Side::Ask, "2", "105", "0"), 1_000)]);
        let cashflows = rows(&g, vec![(cashflow(A, "-0.5"), 1_000)]);
        let positions = fills.position(&settlements, &cashflows, measure, read_positions);

        let zero = Qty::ZERO;
        assert_eq!(
            run(positions, &g),
            vec![
                (t(1_000), vec![(A, zero, q("10"), q("1"), q("-0.5"))]),
                (
                    t(2_000),
                    vec![
                        (A, zero, q("10"), q("1"), q("-0.5")),
                        (B, q("-1"), zero, zero, zero)
                    ]
                ),
            ]
        );
    }

    /// The short form leaving both optional inputs out is the long form
    /// handed streams that never tick: the same readings at the same
    /// instants. A fill nothing values still moves the net.
    #[test]
    fn the_short_form_is_the_long_form_with_inputs_that_never_tick() {
        let script = vec![
            (fill(A, Side::Bid, "3", "100", "0"), 1_000),
            (fill(SWAP, Side::Bid, "1", "7", "0"), 1_000),
            (fill(A, Side::Ask, "1", "110", "0"), 3_000),
        ];
        let g = GraphBuilder::new();
        let short = rows(&g, script.clone()).wire_position(measure).build();
        let short = run(short, &g);

        let g = GraphBuilder::new();
        let settlements = g.never().map(|(): &()| Burst::<Fill<Ticker>>::new());
        let cashflows = g.never().map(|(): &()| Burst::<Cashflow<Ticker>>::new());
        let long = rows(&g, script).position(&settlements, &cashflows, measure, read_positions);
        let long = run(long, &g);

        let zero = Qty::ZERO;
        assert_eq!(
            short,
            vec![
                (
                    t(1_000),
                    vec![
                        (A, q("3"), zero, zero, zero),
                        (SWAP, q("1"), zero, zero, zero)
                    ]
                ),
                (
                    t(3_000),
                    vec![
                        (A, q("2"), q("10"), zero, zero),
                        (SWAP, q("1"), zero, zero, zero)
                    ]
                ),
            ]
        );
        assert_eq!(short, long);
    }

    /// A reading of the caller's own: the net in one instrument and the
    /// unvalued count, ticking on every cycle the node runs — a cashflow
    /// alone runs it.
    #[test]
    fn a_reading_is_emitted_every_cycle_the_node_runs() {
        let g = GraphBuilder::new();
        let fills = rows(
            &g,
            vec![
                (fill(A, Side::Bid, "1", "100", "0"), 1_000),
                (fill(SWAP, Side::Bid, "1", "7", "0"), 3_000),
            ],
        );
        let cashflows = rows(&g, vec![(cashflow(A, "2"), 2_000)]);
        let read = fills
            .wire_position(measure)
            .cashflows(&cashflows)
            .reading(|book: &Book<Ticker>| (book.net(&A), book.unvalued()))
            .build()
            .with_time()
            .accumulate();
        let mut runner = g.build();
        runner
            .run(RunMode::HistoricalFrom(NanoTime::ZERO), RunFor::Forever)
            .unwrap();
        assert_eq!(
            runner.value(&read),
            vec![
                (t(1_000), (q("1"), 0)),
                (t(2_000), (q("1"), 0)),
                (t(3_000), (q("1"), 1)),
            ]
        );
    }
}
