//! The whole execution layer in one graph: a quoter, the OMS, the ceiling,
//! the venue, the position fold, reconciliation and the kill switch.
//!
//! `cargo run -p wingfoil --example execution_strategy_loop --features execution-testing`
//!
//! A toy quoter rests a tick outside the touch on one future. Its desireds
//! go into the OMS node (`OmsOps`); what the OMS sends goes down a
//! millisecond of wire, through the one feedback cut, past `ceiling::Capped`
//! and into `testing::SimVenue`, whose reports come back up the same wire.
//! Fills fold into a `Book`; the venue's holdings are reconciled against
//! it; the kill switch latches on a mismatch or the day's loss, and its
//! halt is the OMS's `cancel_all` and the quoter's reason to stop. An
//! operator's `clear` reopens, and the next touch re-quotes.
//!
//! This is the wiring a venue integration copies: every edge here is the
//! one it would have live, and the same graph runs historically.

use std::time::Duration;

use anyhow::Result;
use wingfoil::adapters::execution::ceiling::{Cap, Capped};
use wingfoil::adapters::execution::edge::{Held, Holdings, Report, Request};
use wingfoil::adapters::execution::kill_switch::{Breaches, Limit, Stance, Switch};
use wingfoil::adapters::execution::oms::{Config, Desired, OmsOps};
use wingfoil::adapters::execution::order::{Epoch, Fill};
use wingfoil::adapters::execution::position::{Book, Measure, Pnl};
use wingfoil::adapters::execution::rate_limit::{OrderRate, Terms};
use wingfoil::adapters::execution::reconcile::{self, Fold, Mismatch, Reconciler};
use wingfoil::adapters::execution::testing::{Profile, SimVenue, Touch};
use wingfoil::adapters::execution::venue::Venue;
use wingfoil::adapters::market::{Level, Px, Qty};
use wingfoil::op::{Activation, Ctx, Op, Tick};
use wingfoil::prelude::*;
use wingfoil::{NanoTime, RunFor, RunMode};

/// The E-mini S&P future: $50 a point.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash, PartialOrd, Ord)]
struct Es;

const START: NanoTime = NanoTime::ZERO;
/// The wire to the venue, each way — and the market data's way back.
const WIRE: Duration = Duration::from_millis(1);
/// The sweep: the clock the OMS re-plans on and the reconciler judges on.
const SWEEP: Duration = Duration::from_millis(50);

const RATE: OrderRate = OrderRate::stated(Terms::per_second(50, 50), Terms::per_second(1, 1), 1.0);
const CONFIG: Config = Config::unmetered()
    .with_rate(RATE)
    .with_max_desired_age(Duration::from_secs(60));
const VENUE: Profile = Profile {
    post_only: true,
    mass_cancel: true,
};
/// A difference with the venue is a mismatch once it has stood this long.
const GRACE: Duration = Duration::from_millis(100);
/// The day's equity at the open, and how much of it may be lost.
const OPENING_EQUITY: &str = "100000";
const DAILY_LOSS: f64 = 0.02;

fn at(ms: u64) -> NanoTime {
    START + Duration::from_millis(ms)
}

fn ms(t: NanoTime) -> u64 {
    u64::from(t - START) / 1_000_000
}

fn px(s: &str) -> Px {
    Px::parse(s).unwrap()
}

fn q(s: &str) -> Qty {
    Qty::parse(s).unwrap()
}

/// `price` moved by `ticks` quarter-points.
fn ticks(price: Px, ticks: i128) -> Px {
    Px::from_raw(price.raw() + ticks * px("0.25").raw())
}

fn touch(bid: &str, ask: &str) -> Touch<Es> {
    Touch {
        instrument: Es,
        bid: Some(px(bid)),
        ask: Some(px(ask)),
    }
}

fn replay<T: Clone + Default + 'static>(
    g: &GraphBuilder,
    rows: Vec<(T, NanoTime)>,
) -> Stream<Burst<T>> {
    g.replay_results(rows.into_iter().map(Ok))
}

// --- The quoter ------------------------------------------------------------

/// One lot a tick outside the touch, both sides — or nothing unless the
/// switch is open.
fn quote(now: NanoTime, touch: &Touch<Es>, stance: Stance) -> Desired<Es> {
    match (stance, touch.bid, touch.ask) {
        (Stance::Open, Some(bid), Some(ask)) => Desired::two_way(
            Es,
            now,
            Level::new(ticks(bid, -1), q("1")),
            Level::new(ticks(ask, 1), q("1")),
        ),
        _ => Desired::nothing(Es, now),
    }
}

// --- The ceiling -----------------------------------------------------------

/// At most five lots an order.
#[derive(Clone, Copy, Debug)]
struct Clip;

impl Cap<Es> for Clip {
    fn of(&self, _: &Es) -> Option<Qty> {
        Some(q("5"))
    }
}

// --- The desk: the position fold and its reconciliation --------------------

/// The book and the mark a re-base is booked at: the caller's [`Fold`].
struct Desk {
    book: Book<Es>,
    mark: Option<Px>,
}

impl Fold<Es> for Desk {
    fn positions(&self) -> &Book<Es> {
        &self.book
    }

    fn rebase(&mut self, _now: NanoTime, instrument: Es, net: Qty) {
        self.book.rebase(instrument, net, self.mark);
    }
}

/// What the desk reads after one cycle.
#[derive(Clone, Debug, Default)]
struct DeskReading {
    net: Qty,
    entry: Option<Px>,
    mark: Option<Px>,
    pnl: Option<Pnl>,
    /// Whether the venue has stated its holdings yet.
    heard: bool,
    disagreeing: usize,
    mismatches: Vec<Mismatch<Es>>,
}

/// The desk as an op, fed in this order within an instant: fills (folded —
/// an ordinary fill is not a settlement), settlements (folded, and told to
/// the reconciler: the venue closed the position, so its side is flat), the
/// venue's holdings, the touch (the mark), then `reconcile` at the engine
/// time on every cycle — the sweep's included, which is what judges the
/// grace.
struct DeskOp;

#[wingfoil::op(build = desk_op)]
impl Op for DeskOp {
    type Cfg = reconcile::Config;
    type State = Option<(Desk, Reconciler<Es>)>;
    type In<'a> = (
        (&'a Burst<Fill<Es>>, bool),
        (&'a Burst<Fill<Es>>, bool),
        (&'a Holdings<Es>, bool),
        (&'a Burst<Touch<Es>>, bool),
        (&'a (), bool),
    );
    type Out = DeskReading;
    const ACTIVATION: Activation = Activation::NONE;

    fn cycle(
        config: &mut reconcile::Config,
        state: &mut Option<(Desk, Reconciler<Es>)>,
        input: Self::In<'_>,
        ctx: &mut Ctx<'_>,
    ) -> Result<Tick<DeskReading>> {
        let (desk, reconciler) = state.get_or_insert_with(|| {
            let desk = Desk {
                book: Book::new(|_| Measure::scaled(q("50"))),
                mark: None,
            };
            (desk, Reconciler::new(*config))
        });
        let (
            (fills, filled),
            (settlements, settled),
            (holdings, stated),
            (touches, touched),
            _sweep,
        ) = input;
        if filled {
            desk.book.apply(fills);
        }
        if settled {
            desk.book.apply(settlements);
            reconciler.settled(settlements);
        }
        if stated {
            reconciler.heard(holdings);
        }
        if touched {
            for touch in touches {
                if let (Some(bid), Some(ask)) = (touch.bid, touch.ask) {
                    desk.mark = Some(Px::midpoint(bid, ask));
                }
            }
        }
        let mismatches = reconciler.reconcile(ctx.time(), desk);
        let position = desk.book.position(&Es);
        Ok(Tick::Value(DeskReading {
            net: desk.book.net(&Es),
            entry: position.and_then(|p| p.entry()),
            mark: desk.mark,
            pnl: desk.book.total(|_| true, |_| desk.mark),
            heard: reconciler.has_heard(),
            disagreeing: reconciler.disagreeing(),
            mismatches,
        }))
    }
}

trait DeskOps {
    fn desk(
        &self,
        settlements: &Stream<Burst<Fill<Es>>>,
        holdings: &Stream<Holdings<Es>>,
        marks: &Stream<Burst<Touch<Es>>>,
        sweep: &Stream<()>,
        config: reconcile::Config,
    ) -> Stream<DeskReading>;
}

impl DeskOps for Stream<Burst<Fill<Es>>> {
    fn desk(
        &self,
        settlements: &Stream<Burst<Fill<Es>>>,
        holdings: &Stream<Holdings<Es>>,
        marks: &Stream<Burst<Touch<Es>>>,
        sweep: &Stream<()>,
        config: reconcile::Config,
    ) -> Stream<DeskReading> {
        let (settlements, holdings, marks, sweep) = (
            settlements.handle(),
            holdings.handle(),
            marks.handle(),
            sweep.handle(),
        );
        self.wire(move |b, fills| b.desk_op(fills, settlements, holdings, marks, sweep, config))
    }
}

// --- The kill switch -------------------------------------------------------

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
enum Risk {
    /// The day's loss past its cap, or an equity nobody could state.
    DailyLoss,
    /// The venue and the fold disagreed for longer than the grace.
    Mismatch,
}

impl Limit for Risk {
    const ALL: &'static [Risk] = &[Risk::DailyLoss, Risk::Mismatch];
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
struct Assessed {
    stance: Stance,
    standing: Breaches<Risk>,
    latched: Breaches<Risk>,
}

/// The switch as an op: the desk's reading assessed and recorded, then
/// the operator's `clear`, which refuses while a breach stands.
struct SwitchOp;

#[wingfoil::op(build = switch_op)]
impl Op for SwitchOp {
    type Cfg = ();
    type State = Option<Switch<Risk>>;
    type In<'a> = ((&'a DeskReading, bool), (&'a (), bool));
    type Out = Assessed;
    const ACTIVATION: Activation = Activation::NONE;

    fn cycle(
        _: &mut (),
        state: &mut Option<Switch<Risk>>,
        input: Self::In<'_>,
        ctx: &mut Ctx<'_>,
    ) -> Result<Tick<Assessed>> {
        // The day starts at the run's own midnight: engine time zero.
        let switch = state.get_or_insert_with(|| Switch::new(Duration::ZERO));
        let ((reading, read), (_, clear)) = input;
        let now = ctx.time();
        if read {
            let mut standing = Breaches::NONE;
            let equity = reading.pnl.map(|pnl| q(OPENING_EQUITY) + pnl.net());
            // One-sided: a winning day is not a breach, and a NaN is.
            if !switch
                .daily_loss(now, equity)
                .is_some_and(|(fraction, _)| fraction <= DAILY_LOSS)
            {
                standing.set(Risk::DailyLoss);
            }
            if !reading.mismatches.is_empty() {
                standing.set(Risk::Mismatch);
            }
            switch.record(now, standing);
        }
        if clear {
            // Refused while a breach stands; the reading that follows says so.
            let _ = switch.clear();
        }
        Ok(Tick::Value(Assessed {
            stance: switch.stance(),
            standing: switch.standing(),
            latched: switch.latched(),
        }))
    }
}

trait SwitchOps {
    fn kill_switch(&self, clear: &Stream<()>) -> Stream<Assessed>;
}

impl SwitchOps for Stream<DeskReading> {
    fn kill_switch(&self, clear: &Stream<()>) -> Stream<Assessed> {
        let clear = clear.handle();
        self.wire(move |b, reading| b.switch_op(reading, clear))
    }
}

// --- Printing --------------------------------------------------------------

fn describe(request: &Request<Es>) -> String {
    match request {
        Request::Place(o) => format!(
            "place  {} {:?} {} @ {}",
            o.id.sequence(),
            o.side,
            o.qty,
            o.price.map_or("market".to_string(), |p| p.to_string())
        ),
        Request::Amend(a) => format!("amend  {} → {} @ {}", a.order.sequence(), a.qty, a.price),
        Request::Cancel(id) => format!("cancel {}", id.sequence()),
        Request::CancelAll => "cancel all".to_string(),
    }
}

fn heard(report: &Report<Es>) -> String {
    match report {
        Report::Ack(a) => format!("ack       {}", a.order.sequence()),
        Report::Reject(r) => format!("reject    {:?}", r.reason),
        Report::Fill(f) => format!(
            "fill      {} {} @ {} ({:?})",
            f.order.sequence(),
            f.qty,
            f.price,
            f.liquidity
        ),
        Report::Cancelled(r) => format!("cancelled {}", r.order.sequence()),
        Report::Expired(r) => format!("expired   {}", r.order.sequence()),
        Report::Triggered(f) => format!("triggered {}", f.order.sequence()),
    }
}

fn main() -> Result<()> {
    let g = GraphBuilder::new();

    // The market at the venue: it trades down through the bid at 100ms and
    // recovers once the book is halted.
    let touches = replay(
        &g,
        vec![
            (touch("5000.00", "5000.25"), at(0)),
            (touch("4999.50", "4999.75"), at(100)),
            (touch("5000.00", "5000.25"), at(420)),
            (touch("5000.00", "5000.25"), at(460)),
        ],
    );
    // What the venue says it holds, 300ms in: two lots, where the fold has
    // one — a fill lost on the way, say. SimVenue states no holdings, so
    // this stands in for `session.holdings`; live, that stream goes here.
    let holdings = replay(
        &g,
        vec![(
            Holdings {
                whole: true,
                held: [Held {
                    instrument: Es,
                    net: q("2"),
                }]
                .into_iter()
                .collect(),
                as_of: at(300),
            },
            at(300),
        )],
    )
    .filter_map(|burst: &Burst<Holdings<Es>>| burst.last().cloned());
    // The operator clears the switch at 450ms.
    let clear = replay(&g, vec![((), at(450))]).map(|_: &Burst<()>| ());
    let sweep = g.ticker(SWEEP);

    // Market data takes the wire too: the quoter sees each touch a
    // millisecond after the venue does.
    let seen = touches.delay(WIRE);

    // The cut: what the OMS sends at one instant lands at the venue a wire
    // later, through the ceiling; the venue's reports come back up the
    // same wire.
    let (landed, cut) = g.feedback::<Burst<Request<Es>>>();
    let venue = SimVenue::new(VENUE, Epoch::ZERO, &touches);
    let session = Capped::new(&venue, Clip).wire(&landed.delay(WIRE));
    let reports = session.reports.delay(WIRE);

    // Fills fold into the desk, whose reading the switch assesses; a halt
    // is both the OMS's cancel-all and the quoter's reason to want nothing.
    // All of it reads the venue's reports, which are on the far side of the
    // cut, so closing the loop through the switch adds no second cut.
    let fills = reports
        .map(|reports: &Burst<Report<Es>>| {
            reports
                .iter()
                .filter_map(|report| match report {
                    Report::Fill(fill) => Some(*fill),
                    _ => None,
                })
                .collect::<Burst<Fill<Es>>>()
        })
        .filter_map(|fills: &Burst<Fill<Es>>| (!fills.is_empty()).then(|| fills.clone()));
    // Settlements are the venue's own edge, up the same wire (SimVenue
    // leaves it quiet).
    let settlements = session.settlements.delay(WIRE);
    let desk = fills.desk(
        &settlements,
        &holdings,
        &seen,
        &sweep,
        reconcile::Config { grace: GRACE },
    );
    let assessed = desk.kill_switch(&clear);
    // The stance, not a bool: `Stance::default()` is `Halted`, so nothing
    // reads as open before the switch has assessed anything.
    let stance = assessed.map(|a| a.stance);
    let cancel_all = stance
        .distinct()
        .filter_map(|stance| (*stance == Stance::Halted).then_some(()));

    // The quoter decides on each touch it sees, reading the switch
    // passively; the OMS closes the gap to what is working.
    let desired = seen
        .with_time()
        .join_passive(&stance, |(now, touches), stance| {
            touches
                .iter()
                .map(|touch| quote(*now, touch, *stance))
                .collect::<Burst<Desired<Es>>>()
        });
    let (requests, _pacing) = desired
        .wire_oms(CONFIG, Epoch::ZERO, &reports)
        .sweep(&sweep)
        .cancel_all(&cancel_all)
        .build();
    let requests = requests.feedback(&cut);

    let _sent = requests.with_time().for_each(|(t, burst)| {
        for request in burst {
            println!("t={:>3}ms sent   {}", ms(*t), describe(request));
        }
        Ok(())
    });
    let _heard = reports.with_time().for_each(|(t, burst)| {
        for report in burst {
            println!("t={:>3}ms heard  {}", ms(*t), heard(report));
        }
        Ok(())
    });
    let _desk = desk
        .map(|r| (r.net, r.entry, r.heard, r.disagreeing, r.mismatches.len()))
        .distinct()
        .join_passive(&desk, |_, r| r.clone())
        .with_time()
        .for_each(|(t, r)| {
            for m in &r.mismatches {
                println!(
                    "t={:>3}ms desk   mismatch: ours {}, venue {} since t={}ms → re-based",
                    ms(*t),
                    m.ours,
                    m.venue,
                    ms(m.since)
                );
            }
            let standing = match (r.heard, r.disagreeing) {
                (false, _) => "venue silent".to_string(),
                (true, 0) => "agrees with the venue".to_string(),
                (true, n) => format!("{n} position disagrees, inside the grace"),
            };
            println!(
                "t={:>3}ms desk   net {} entry {} mark {} pnl {} — {standing}",
                ms(*t),
                r.net,
                r.entry.map_or("-".to_string(), |p| p.to_string()),
                r.mark.map_or("-".to_string(), |p| p.to_string()),
                r.pnl.map_or("-".to_string(), |p| p.net().to_string()),
            );
            Ok(())
        });
    let _switch = assessed.distinct().with_time().for_each(|(t, a)| {
        println!(
            "t={:>3}ms switch {:?}: standing {:?}, latched {:?}",
            ms(*t),
            a.stance,
            a.standing,
            a.latched
        );
        Ok(())
    });

    let mut runner = g.build();
    runner.run(
        RunMode::HistoricalFrom(START),
        RunFor::Duration(Duration::from_millis(500)),
    )?;
    Ok(())
}
