//! The kill switch over limits of your own, and a per-order ceiling, each
//! in a graph.
//!
//! `cargo run -p wingfoil --example execution_latch_and_cap --features execution`
//!
//! An equity book with three limits — gross exposure, the day's loss, and
//! a feed that went quiet. `Switch` owns the latch: a breach halts until an
//! operator clears it, `clear` refuses while one stands, and a restart
//! carries it. The caps and what they compare are the caller's.
//!
//! The switch is node state: the book's readings and the operator's
//! `clear` are replayed at engine instants into `SwitchOp`, and the
//! restart is a second graph started from the first one's `Snapshot`. The
//! ceiling is `ceiling::Capped` around a venue on the request wire: the
//! first order over its cap aborts the run.

use std::time::Duration;

use anyhow::Result;
use wingfoil::adapters::execution::ceiling::{Cap, Capped};
use wingfoil::adapters::execution::edge::{Ack, Report, Request};
use wingfoil::adapters::execution::kill_switch::{
    Breaches, ClearError, Limit, Snapshot, Stance, Switch, within,
};
use wingfoil::adapters::execution::order::{ClientOrderId, Order};
use wingfoil::adapters::execution::venue::{Session, Venue};
use wingfoil::adapters::market::{Px, Qty, Side};
use wingfoil::op::{Activation, Ctx, Op, Tick};
use wingfoil::prelude::*;
use wingfoil::{NanoTime, RunFor, RunMode};

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
enum Risk {
    Gross,
    DailyLoss,
    Stale,
}

impl Limit for Risk {
    const ALL: &'static [Risk] = &[Risk::Gross, Risk::DailyLoss, Risk::Stale];
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash, PartialOrd, Ord)]
enum Ticker {
    #[default]
    Aapl,
    Penny,
}

/// What the book reads at one instant: the caller's inputs to its caps.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
struct Reading {
    gross: f64,
    equity: Option<Qty>,
    fresh: bool,
}

/// What the switch said after one cycle.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
struct Assessed {
    stance: Stance,
    standing: Breaches<Risk>,
    latched: Breaches<Risk>,
    latched_at: Option<NanoTime>,
    opening_equity: Option<Qty>,
    /// The operator's `clear`, where one was asked for this cycle.
    cleared: Option<Result<(), ClearError<Risk>>>,
    /// What a restart carries.
    snapshot: Snapshot<Risk>,
}

/// The switch as an op: a [`Switch`] held as node state, fed in this order
/// within an instant —
///
/// 1. **readings** — the caller's caps over what it computed, recorded;
/// 2. **clear** — the operator's, which refuses while a breach stands.
///
/// - `Cfg` — the day's start, and the [`Snapshot`] a restarted process
///   carries in, if any.
/// - `State` — the switch, built or restored on the first cycle.
/// - `Out` — what it said.
struct SwitchOp;

#[wingfoil::op(build = switch_op)]
impl Op for SwitchOp {
    type Cfg = (Duration, Option<Snapshot<Risk>>);
    type State = Option<Switch<Risk>>;
    type In<'a> = ((&'a Burst<Reading>, bool), (&'a (), bool));
    type Out = Assessed;
    const ACTIVATION: Activation = Activation::NONE;

    fn cycle(
        cfg: &mut (Duration, Option<Snapshot<Risk>>),
        state: &mut Option<Switch<Risk>>,
        input: Self::In<'_>,
        ctx: &mut Ctx<'_>,
    ) -> Result<Tick<Assessed>> {
        let switch = state.get_or_insert_with(|| match cfg.1 {
            Some(snapshot) => Switch::restore(cfg.0, snapshot),
            None => Switch::new(cfg.0),
        });
        let ((readings, read), (_, clear)) = input;
        let now = ctx.time();
        if read {
            for reading in readings {
                let standing = assess(switch, now, reading);
                switch.record(now, standing);
            }
        }
        let cleared = clear.then(|| switch.clear());
        Ok(Tick::Value(Assessed {
            stance: switch.stance(),
            standing: switch.standing(),
            latched: switch.latched(),
            latched_at: switch.latch().map(|l| l.at),
            opening_equity: switch.opening_equity(),
            cleared,
            snapshot: switch.snapshot(),
        }))
    }
}

/// One assessment: the caller's caps over what it computed.
fn assess(switch: &mut Switch<Risk>, now: NanoTime, reading: &Reading) -> Breaches<Risk> {
    let mut standing = Breaches::NONE;
    if !within(reading.gross, 1_000_000.0) {
        standing.set(Risk::Gross);
    }
    // An equity nobody knows is a breach, not a pass.
    if !switch
        .daily_loss(now, reading.equity)
        .is_some_and(|(fraction, _)| fraction <= 0.02)
    {
        standing.set(Risk::DailyLoss);
    }
    if !reading.fresh {
        standing.set(Risk::Stale);
    }
    standing
}

/// The fluent form: the switch over this stream of readings, cleared on
/// `clear`.
trait SwitchOps {
    fn kill_switch(
        &self,
        clear: &Stream<()>,
        day_start: Duration,
        restored: Option<Snapshot<Risk>>,
    ) -> Stream<Assessed>;
}

impl SwitchOps for Stream<Burst<Reading>> {
    fn kill_switch(
        &self,
        clear: &Stream<()>,
        day_start: Duration,
        restored: Option<Snapshot<Risk>>,
    ) -> Stream<Assessed> {
        let clear = clear.handle();
        self.wire(move |b, readings| b.switch_op(readings, clear, (day_start, restored)))
    }
}

/// At most 500 shares an order, and nothing at all in penny stocks.
#[derive(Clone, Copy, Debug)]
struct Clip;

impl Cap<Ticker> for Clip {
    fn of(&self, instrument: &Ticker) -> Option<Qty> {
        (*instrument == Ticker::Aapl).then(|| Qty::parse("500").unwrap())
    }
}

/// A venue that acks whatever reaches it: what matters here is what the
/// ceiling lets through.
struct Acking;

impl Venue<Ticker> for Acking {
    fn wire(&self, requests: &Stream<Burst<Request<Ticker>>>) -> Session<Ticker> {
        let acks = requests.with_time().map(|(now, requests)| {
            requests
                .iter()
                .filter_map(Request::order)
                .map(|order| {
                    Report::Ack(Ack {
                        order,
                        recv_time: *now,
                        ..Ack::default()
                    })
                })
                .collect::<Burst<Report<Ticker>>>()
        });
        Session::quiet(acks)
    }
}

/// US equities: the day starts at the 09:30 ET open, 13:30 UTC.
const OPEN: Duration = Duration::from_secs(13 * 3600 + 30 * 60);

/// Day 20,000 since the epoch, at the open.
fn start() -> NanoTime {
    NanoTime::ZERO + Duration::from_secs(20_000 * 86_400) + OPEN
}

fn at(secs: u64) -> NanoTime {
    start() + Duration::from_secs(secs)
}

fn secs(t: NanoTime) -> u64 {
    u64::from(t - start()) / 1_000_000_000
}

fn reading(gross: f64, equity: &str, fresh: bool, at: NanoTime) -> (Reading, NanoTime) {
    let reading = Reading {
        gross,
        equity: Some(Qty::parse(equity).unwrap()),
        fresh,
    };
    (reading, at)
}

fn replay<T: Clone + Default + 'static>(
    g: &GraphBuilder,
    rows: Vec<(T, NanoTime)>,
) -> Stream<Burst<T>> {
    g.replay_results(rows.into_iter().map(Ok))
}

fn show(t: NanoTime, a: &Assessed) {
    let mut line = format!(
        "t={:>3}s {:?}: standing {:?}",
        secs(t),
        a.stance,
        a.standing
    );
    if let Some(since) = a.latched_at {
        line += &format!(", latched {:?} since t={}s", a.latched, secs(since));
    }
    if let Some(cleared) = a.cleared {
        line += &format!("; clear → {cleared:?}");
    }
    println!("{line}");
}

/// The switch over a day's readings, from the open or from a snapshot;
/// what it carried at the end of the run.
fn day(
    readings: Vec<(Reading, NanoTime)>,
    clears: Vec<NanoTime>,
    restored: Option<Snapshot<Risk>>,
    until: Duration,
) -> Result<Snapshot<Risk>> {
    let g = GraphBuilder::new();
    let readings = replay(&g, readings);
    let clear = replay(&g, clears.into_iter().map(|at| ((), at)).collect()).map(|_: &Burst<()>| ());
    let assessed = readings.kill_switch(&clear, OPEN, restored);
    let _shown = assessed.with_time().for_each(|(t, a)| {
        show(*t, a);
        Ok(())
    });
    let mut runner = g.build();
    runner.run(RunMode::HistoricalFrom(start()), RunFor::Duration(until))?;
    Ok(runner.value(&assessed).snapshot)
}

fn main() -> Result<()> {
    // The day: inside at the open; a gap — gross exposure through its cap
    // and 3% down on the day; the market back, with the latch outliving it;
    // then a NaN exposure, which is outside every cap, and a stale feed,
    // with the operator's clear refused while they stand.
    let readings = vec![
        reading(400_000.0, "1000000", true, at(0)),
        reading(1_200_000.0, "970000", true, at(60)),
        reading(500_000.0, "995000", true, at(120)),
        reading(f64::NAN, "995000", false, at(180)),
    ];
    println!("the day:");
    let snapshot = day(readings, vec![at(180)], None, Duration::from_secs(200))?;
    println!(
        "  carried: latched {:?} since t={}s, standing {:?}, opening equity {}",
        snapshot.latch.map_or(Breaches::NONE, |l| l.breaches),
        snapshot.latch.map_or(0, |l| secs(l.at)),
        snapshot.standing,
        snapshot
            .day
            .map_or("-".to_string(), |d| d.opening.to_string())
    );

    // A restart carries the latch and the day; it does not release them.
    // Inside again, an operator clears it.
    println!("restarted from the snapshot:");
    let readings = vec![reading(500_000.0, "995000", true, at(240))];
    day(
        readings,
        vec![at(245)],
        Some(snapshot),
        Duration::from_secs(250),
    )?;

    // The ceiling: per order, before the venue. 500 AAPL is inside the cap
    // and acked; 501 is over it, and the run ends there rather than the
    // order being dropped or resized.
    println!("the ceiling:");
    let g = GraphBuilder::new();
    let order = |id: u64, ticker, size: &str, at| {
        let order = Order::limit(
            ClientOrderId(id),
            ticker,
            Side::Bid,
            Qty::parse(size).unwrap(),
            Px::parse("190").unwrap(),
        );
        (Request::Place(order), at)
    };
    let requests = replay(
        &g,
        vec![
            order(1, Ticker::Aapl, "500", at(1)),
            order(2, Ticker::Aapl, "501", at(2)),
            order(3, Ticker::Penny, "1", at(3)),
        ],
    );
    let session = Capped::new(&Acking, Clip).wire(&requests);
    let _heard = session.reports.with_time().for_each(|(t, reports)| {
        for report in reports {
            if let Report::Ack(ack) = report {
                println!("t={:>3}s order {} acked", secs(*t), ack.order.0);
            }
        }
        Ok(())
    });
    let mut runner = g.build();
    match runner.run(RunMode::HistoricalFrom(start()), RunFor::Forever) {
        Ok(()) => println!("  the run completed"),
        Err(error) => println!("  the run aborted: {error:#}"),
    }
    Ok(())
}
