//! The whole execution layer in one graph: a quoter, the OMS, the ceiling,
//! the venue, the position fold, reconciliation and the kill switch.
//!
//! `cargo run -p wingfoil --example execution_strategy_loop --features execution-testing`
//!
//! A toy quoter rests a tick outside the touch on one future. Its desireds
//! go into the OMS node (`OmsOps`); what the OMS sends goes down a
//! millisecond of wire, through the one feedback cut, past `ceiling::Capped`
//! and into `testing::SimVenue`, whose reports come back up the same wire.
//! Fills fold into a `Book` and the venue's holdings are reconciled against
//! it, in one library node (`ReconcileOps`); the kill switch
//! (`KillSwitchOps`) latches on a mismatch or the day's loss, and its halt
//! is the OMS's `cancel_all` and the quoter's reason to stop. An
//! operator's `clear` reopens, and the next touch re-quotes.
//!
//! This is the wiring a venue integration copies: every edge here is the
//! one it would have live, and the same graph runs historically.

use std::time::Duration;

use anyhow::Result;
use wingfoil::adapters::execution::ceiling::{Cap, Capped};
use wingfoil::adapters::execution::edge::{Held, Holdings, Report, Request};
use wingfoil::adapters::execution::kill_switch::{
    Assessed, Breaches, KillSwitchOps, Limit, Stance, StanceOps, Switch,
};
use wingfoil::adapters::execution::oms::{Config, Desired, OmsOps};
use wingfoil::adapters::execution::order::{Epoch, Fill};
use wingfoil::adapters::execution::position::{Measure, Pnl};
use wingfoil::adapters::execution::rate_limit::{OrderRate, Terms};
use wingfoil::adapters::execution::reconcile::node::Desk;
use wingfoil::adapters::execution::reconcile::{self, Mismatch, ReconcileOps};
use wingfoil::adapters::execution::testing::{Profile, SimVenue, Touch};
use wingfoil::adapters::execution::venue::Venue;
use wingfoil::adapters::market::{Level, Px, Qty, px, qty};
use wingfoil::prelude::*;
use wingfoil::{NanoTime, RunFor, RunMode};

/// The E-mini S&P future: $50 a point.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash, PartialOrd, Ord)]
struct Es;

const START: NanoTime = NanoTime::ZERO;
/// The latency to the venue, each way — and the market data's way back.
const LATENCY: Duration = Duration::from_millis(1);

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
const OPENING_EQUITY: Qty = qty!(100_000);
const DAILY_LOSS: f64 = 0.02;

fn at(ms: u64) -> NanoTime {
    START + Duration::from_millis(ms)
}

fn ms(t: NanoTime) -> u64 {
    u64::from(t - START) / 1_000_000
}

/// `price` moved by `ticks` quarter-points.
fn ticks(price: Px, ticks: i128) -> Px {
    Px::from_raw(price.raw() + ticks * px!(0.25).raw())
}

fn touch(bid: Px, ask: Px) -> Touch<Es> {
    Touch {
        instrument: Es,
        bid: Some(bid),
        ask: Some(ask),
    }
}

// --- The quoter ------------------------------------------------------------

/// One lot a tick outside the touch, both sides — or nothing unless the
/// switch is open.
fn quote(now: NanoTime, touch: &Touch<Es>, stance: Stance) -> Desired<Es> {
    match (stance, touch.bid, touch.ask) {
        (Stance::Open, Some(bid), Some(ask)) => Desired::two_way(
            Es,
            now,
            Level::new(ticks(bid, -1), qty!(1)),
            Level::new(ticks(ask, 1), qty!(1)),
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
        Some(qty!(5))
    }
}

// --- The desk and the kill switch -----------------------------------------

/// What the strategy reads off the reconciliation node after each cycle:
/// the position, its mark and PnL, where reconciliation stands, and what
/// this cycle's reconcile found.
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

fn read_desk(desk: &Desk<Es>) -> DeskReading {
    let (book, reconciler) = (desk.book(), desk.reconciler());
    DeskReading {
        net: book.net(&Es),
        entry: book.position(&Es).and_then(|p| p.entry()),
        mark: desk.mark(&Es),
        pnl: book.total(|_| true, |i| desk.mark(i)),
        heard: reconciler.has_heard(),
        disagreeing: reconciler.disagreeing(),
        mismatches: desk.mismatches().to_vec(),
    }
}

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

/// The caps over one desk reading.
fn assess(switch: &mut Switch<Risk>, now: NanoTime, reading: &DeskReading) -> Breaches<Risk> {
    let mut standing = Breaches::NONE;
    let equity = reading.pnl.map(|pnl| OPENING_EQUITY + pnl.net());
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
    standing
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

/// Everything this graph is fed from outside it, replayed from rows here.
/// Live, each field comes from its real source instead — the touches off a
/// market-data feed, the holdings off the venue's session, the clear from
/// an operator — and the wiring in `main` does not change. The quoter is
/// not an input: it is in the graph.
struct Inputs {
    /// The market at the venue.
    touches: Stream<Burst<Touch<Es>>>,
    /// What the venue says it holds.
    holdings: Stream<Burst<Holdings<Es>>>,
    /// An operator clearing the kill switch.
    clear: Stream<()>,
}

impl Inputs {
    fn replayed(g: &GraphBuilder) -> Self {
        // The market at the venue: it trades down through the bid at 100ms and
        // recovers once the book is halted.
        let touches = g.replay(vec![
            (touch(px!(5000.00), px!(5000.25)), at(0)),
            (touch(px!(4999.50), px!(4999.75)), at(100)),
            (touch(px!(5000.00), px!(5000.25)), at(420)),
            (touch(px!(5000.00), px!(5000.25)), at(460)),
        ]);
        // What the venue says it holds, 300ms in: two lots, where the fold has
        // one — a fill lost on the way, say. SimVenue states no holdings, so
        // this stands in for `session.holdings`; live, that stream goes here,
        // each snapshot as a burst of one.
        let holdings = g.replay(vec![(
            Holdings {
                whole: true,
                held: [Held {
                    instrument: Es,
                    net: qty!(2),
                }]
                .into_iter()
                .collect(),
                as_of: at(300),
            },
            at(300),
        )]);
        // The operator clears the switch at 450ms.
        let clear = g.replay(vec![((), at(450))]).map(|_: &Burst<()>| ());
        Inputs {
            touches,
            holdings,
            clear,
        }
    }
}

fn main() -> Result<()> {
    let g = GraphBuilder::new();
    let Inputs {
        touches,
        holdings,
        clear,
    } = Inputs::replayed(&g);

    // Market data takes the wire too: the quoter sees each touch a
    // millisecond after the venue does.
    let seen = touches.delay(LATENCY);

    // The loop: what the OMS sends at one instant lands at the venue a wire
    // later, through the ceiling; the venue's reports come back up the
    // same wire. `feedback_loop` cuts the cycle — `sent_requests` is what the OMS
    // sent, one instant late — and everything inside the closure is in it.
    let (requests, (reports, desk, assessed)) = g.feedback_loop(|sent_requests| {
        let venue = SimVenue::new(VENUE, Epoch::ZERO, &touches);
        let session = Capped::new(&venue, Clip).wire(&sent_requests.delay(LATENCY));
        let reports = session.reports.delay(LATENCY);

        // Fills fold into the reconciliation node — the book and the reconciler
        // together, because the reconciler's fold is the book — whose reading
        // the switch assesses; a halt is both the OMS's cancel-all and the
        // quoter's reason to want nothing. All of it reads the venue's reports,
        // which are on the far side of the cut, so closing the loop through the
        // switch adds no second cut.
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
        // leaves it quiet). The mark a re-base is booked at is the touch's mid.
        let settlements = session.settlements.delay(LATENCY);
        let marks = seen.map(|touches: &Burst<Touch<Es>>| {
            touches
                .iter()
                .filter_map(|touch| Some((touch.instrument, Px::midpoint(touch.bid?, touch.ask?))))
                .collect::<Burst<(Es, Px)>>()
        });
        let (_mismatches, desk) = fills
            .wire_reconcile(
                reconcile::Config { grace: GRACE },
                |_: &Es| Measure::scaled(qty!(50)),
                &holdings,
            )
            .settlements(&settlements)
            .marks(&marks)
            .reading(read_desk)
            .build();
        // The day starts at the run's own midnight: engine time zero.
        let assessed: Stream<Assessed<Risk>> = desk
            .map(|reading: &DeskReading| std::iter::once(reading.clone()).collect::<Burst<_>>())
            .kill_switch(&clear, Duration::ZERO, None, assess);
        // The stance, not a bool: `Stance::default()` is `Halted`, so nothing
        // reads as open before the switch has assessed anything.
        let stance = assessed.stance();
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
            .cancel_all(&cancel_all)
            .build();
        FeedbackLoop {
            feedback: requests,
            out: (reports, desk, assessed),
        }
    });

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
    let _switch = assessed
        .map(|a: &Assessed<Risk>| (a.stance, a.standing, a.latched))
        .distinct()
        .with_time()
        .for_each(|(t, (stance, standing, latched))| {
            println!(
                "t={:>3}ms switch {stance:?}: standing {standing:?}, latched {latched:?}",
                ms(*t)
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
