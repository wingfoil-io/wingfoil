//! Position reconciliation in a graph: a book read against what the venue
//! says it holds, and moved to the venue past a grace.
//!
//! `cargo run -p wingfoil --example execution_reconcile --features execution`
//!
//! The venue is the truth. A fill lost with a session, a position held
//! before the process started, a trade placed by hand — none reach the
//! fold, so when the two disagree for longer than the grace, the fold is
//! re-based on the venue at its own mark and the caller is told, to latch
//! its risk.
//!
//! The fold and the `Reconciler` are node state: fills and the venue's
//! `Holdings` are replayed at engine instants into `ReconcileOp`, and a
//! one-second sweep is what judges the grace at engine time with nothing
//! new arriving — the same clock a stalled session would be judged by.

use std::collections::HashMap;
use std::marker::PhantomData;
use std::time::Duration;

use anyhow::Result;
use wingfoil::adapters::execution::edge::{Held, Holdings};
use wingfoil::adapters::execution::exec_id::ExecId;
use wingfoil::adapters::execution::order::{ClientOrderId, Fill, Instrument, Liquidity};
use wingfoil::adapters::execution::position::{Book, Measure, Position};
use wingfoil::adapters::execution::reconcile::{Config, Fold, Mismatch, Reconciler};
use wingfoil::adapters::market::{Px, Qty, Side};
use wingfoil::op::{Activation, Ctx, Op, Tick};
use wingfoil::prelude::*;
use wingfoil::{NanoTime, RunFor, RunMode};

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash, PartialOrd, Ord)]
enum Ticker {
    #[default]
    Aapl,
    Msft,
}

/// A book and the marks a re-base is booked at: the caller's fold.
struct Desk<I> {
    book: Book<I>,
    marks: HashMap<I, Px>,
}

impl<I: Instrument> Fold<I> for Desk<I> {
    fn positions(&self) -> &Book<I> {
        &self.book
    }

    fn rebase(&mut self, _now: NanoTime, instrument: I, net: Qty) {
        let mark = self.marks.get(&instrument).copied();
        self.book.rebase(instrument, net, mark);
    }
}

/// What one cycle of reconciliation found.
#[derive(Clone, Debug, Default)]
struct Reconciled<I> {
    /// Whether the venue has stated its holdings yet; nothing is judged
    /// before it has.
    heard: bool,
    /// Positions the venue disagrees on, inside their grace or not.
    disagreeing: usize,
    /// Differences that outlasted the grace this cycle, each re-based.
    mismatches: Vec<Mismatch<I>>,
    /// Re-based positions in all.
    rebased: u64,
    /// The fold after this cycle.
    positions: Vec<Position<I>>,
}

/// Reconciliation as an op: the caller's [`Fold`] and the [`Reconciler`]
/// held as node state, fed in this order within an instant —
///
/// 1. **fills** — folded into the book (an ordinary fill is not a
///    settlement: the venue's side moves with it, and its next snapshot
///    says so);
/// 2. **holdings** — the venue's snapshots, each heard in order;
/// 3. **sweep** — nothing;
///
/// then [`Reconciler::reconcile`] at the engine time, on every cycle the
/// node runs, the sweep's included: that is what judges the grace with
/// nothing new arriving.
///
/// - `Cfg` — the reconciler's [`Config`] and the marks a re-base is booked
///   at.
/// - `State` — the desk and the reconciler, built on the first cycle.
/// - `Out` — what the cycle found.
struct ReconcileOp<I>(PhantomData<I>);

#[wingfoil::op(build = reconcile_op)]
impl<I> Op for ReconcileOp<I>
where
    I: Instrument + 'static,
{
    type Cfg = (Config, HashMap<I, Px>);
    type State = Option<(Desk<I>, Reconciler<I>)>;
    type In<'a> = (
        (&'a Burst<Fill<I>>, bool),
        (&'a Burst<Holdings<I>>, bool),
        (&'a (), bool),
    );
    type Out = Reconciled<I>;
    const ACTIVATION: Activation = Activation::NONE;

    fn cycle(
        cfg: &mut (Config, HashMap<I, Px>),
        state: &mut Option<(Desk<I>, Reconciler<I>)>,
        input: Self::In<'_>,
        ctx: &mut Ctx<'_>,
    ) -> Result<Tick<Reconciled<I>>> {
        let (desk, reconciler) = state.get_or_insert_with(|| {
            let desk = Desk {
                book: Book::new(|_| Some(Measure::Linear)),
                marks: cfg.1.clone(),
            };
            (desk, Reconciler::new(cfg.0))
        });
        let ((fills, filled), (holdings, stated), _sweep) = input;
        if filled {
            desk.book.apply(fills);
        }
        if stated {
            for snapshot in holdings {
                reconciler.heard(snapshot);
            }
        }
        let mismatches = reconciler.reconcile(ctx.time(), desk);
        Ok(Tick::Value(Reconciled {
            heard: reconciler.has_heard(),
            disagreeing: reconciler.disagreeing(),
            mismatches,
            rebased: reconciler.rebased(),
            positions: desk.book.positions(),
        }))
    }
}

/// The fluent form: this stream of fills reconciled against `holdings`,
/// judged on `sweep`.
trait ReconcileOps<I: Instrument + 'static> {
    fn reconcile(
        &self,
        holdings: &Stream<Burst<Holdings<I>>>,
        sweep: &Stream<()>,
        config: Config,
        marks: HashMap<I, Px>,
    ) -> Stream<Reconciled<I>>;
}

impl<I: Instrument + 'static> ReconcileOps<I> for Stream<Burst<Fill<I>>> {
    fn reconcile(
        &self,
        holdings: &Stream<Burst<Holdings<I>>>,
        sweep: &Stream<()>,
        config: Config,
        marks: HashMap<I, Px>,
    ) -> Stream<Reconciled<I>> {
        let (holdings, sweep) = (holdings.handle(), sweep.handle());
        self.wire(move |b, fills| b.reconcile_op(fills, holdings, sweep, (config, marks)))
    }
}

const START: NanoTime = NanoTime::ZERO;

fn at(secs: u64) -> NanoTime {
    START + Duration::from_secs(secs)
}

fn secs(t: NanoTime) -> u64 {
    u64::from(t - START) / 1_000_000_000
}

fn q(s: &str) -> Qty {
    Qty::parse(s).unwrap()
}

fn holdings(whole: bool, held: &[(Ticker, &str)], at: NanoTime) -> (Holdings<Ticker>, NanoTime) {
    let holdings = Holdings {
        whole,
        held: held
            .iter()
            .map(|(instrument, net)| Held {
                instrument: *instrument,
                net: q(net),
            })
            .collect::<Burst<Held<Ticker>>>(),
        as_of: at,
    };
    (holdings, at)
}

fn replay<T: Clone + Default + 'static>(
    g: &GraphBuilder,
    rows: Vec<(T, NanoTime)>,
) -> Stream<Burst<T>> {
    g.replay_results(rows.into_iter().map(Ok))
}

fn main() -> Result<()> {
    let g = GraphBuilder::new();

    // The one fill the fold saw: 100 AAPL.
    let fills = replay(
        &g,
        vec![(
            Fill {
                order: ClientOrderId(1),
                exec_id: ExecId::new("e1").unwrap(),
                instrument: Ticker::Aapl,
                side: Side::Bid,
                qty: q("100"),
                filled: q("100"),
                remaining: Qty::ZERO,
                price: Px::parse("189").unwrap(),
                fee: Qty::ZERO,
                liquidity: Liquidity::Maker,
                venue_time: None,
                recv_time: at(0),
            },
            at(0),
        )],
    );
    // The venue's snapshot, two seconds in: it agrees on AAPL but holds 30
    // MSFT the fold never saw — a fill lost with a dropped session, say.
    let holdings = replay(
        &g,
        vec![holdings(
            true,
            &[(Ticker::Aapl, "100"), (Ticker::Msft, "30")],
            at(2),
        )],
    );
    let sweep = g.ticker(Duration::from_secs(1));

    let marks = HashMap::from([
        (Ticker::Aapl, Px::parse("190").unwrap()),
        (Ticker::Msft, Px::parse("420").unwrap()),
    ]);
    let reconciled = fills.reconcile(
        &holdings,
        &sweep,
        Config {
            grace: Duration::from_secs(5),
        },
        marks,
    );

    // The standing, whenever it changes.
    let _standing = reconciled
        .map(|r| (r.heard, r.disagreeing, r.rebased))
        .distinct()
        .with_time()
        .for_each(|(t, (heard, disagreeing, rebased))| {
            let line = match (heard, disagreeing, rebased) {
                (false, _, _) => "venue silent: nothing judged".to_string(),
                (true, 0, 0) => "agreed".to_string(),
                (true, 0, n) => format!("agreed; re-based {n} position(s) in all"),
                (true, n, _) => format!("{n} position(s) disagree, inside the grace"),
            };
            println!("t={} {line}", secs(*t));
            Ok(())
        });
    // Each mismatch, as it is found and re-based.
    let _mismatches = reconciled.with_time().for_each(|(t, r)| {
        for m in &r.mismatches {
            println!(
                "t={} mismatch on {:?}: ours {}, venue {}, since t={} → re-based",
                secs(*t),
                m.instrument,
                m.ours,
                m.venue,
                secs(m.since)
            );
            let position = r
                .positions
                .iter()
                .find(|p| p.instrument == m.instrument)
                .expect("a re-based position is in the book");
            println!(
                "     {:?} now net {}, entry {} — the book's own mark",
                m.instrument,
                position.net(),
                position.entry().map_or("-".to_string(), |p| p.to_string())
            );
        }
        Ok(())
    });

    let mut runner = g.build();
    runner.run(
        RunMode::HistoricalFrom(START),
        RunFor::Duration(Duration::from_secs(10)),
    )?;
    Ok(())
}
