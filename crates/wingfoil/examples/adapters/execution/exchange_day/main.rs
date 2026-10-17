//! A trading day at an exchange, in a graph: no post-only, day orders, a
//! halt and a close.
//!
//! `cargo run -p wingfoil --example execution_exchange_day --features execution-testing`
//!
//! `Config::passive = Limit` rests quotes as plain limits where the venue
//! has no post-only; `Config::lifetime = Day` makes them day orders the
//! close expires; and the venue's `TradingState`, which reaches the OMS
//! node through the session, holds all but cancels while the book is not
//! open. The day's states and the desk's decisions are replayed at their
//! instants; the test venue the OMS is opened on answers, and expires what
//! rests when it closes.

use std::time::Duration;

use wingfoil::adapters::execution::edge::TradingState;
use wingfoil::adapters::execution::oms::{Config, Desired, Lifetime, OmsLoopOps, Passive};
use wingfoil::adapters::execution::order::Epoch;
use wingfoil::adapters::execution::rate_limit::{OrderRate, Terms};
use wingfoil::adapters::execution::testing::{Profile, SimVenue, Touch};
use wingfoil::adapters::market::{Level, Side, px, qty};
use wingfoil::prelude::*;
use wingfoil::{NanoTime, RunFor, RunMode};

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash, PartialOrd, Ord)]
enum Stock {
    #[default]
    Aapl,
    Msft,
}

/// The 09:30 open, day one.
const OPEN: NanoTime = NanoTime::ZERO;

/// The latency to the venue, each way.
const LATENCY: Duration = Duration::from_millis(1);

/// Mints the OMS's order ids. A backtest has no earlier process whose ids
/// could collide, so the epoch is zero.
const EPOCH: Epoch = Epoch::ZERO;

const RATE: OrderRate = OrderRate::stated(Terms::per_second(50, 50), Terms::per_second(1, 1), 1.0);

/// Plain limits, day orders, a decision believed for a day.
const CONFIG: Config = Config::unmetered()
    .with_rate(RATE)
    .with_passive(Passive::Limit)
    .with_lifetime(Lifetime::Day)
    .with_max_desired_age(Duration::from_secs(24 * 3600));

/// Minutes after the open.
fn at(minutes: u64) -> NanoTime {
    OPEN + Duration::from_secs(minutes * 60)
}

/// `at` inverted, as a clock on the wall.
fn clock(t: NanoTime) -> String {
    let wall = 9 * 60 + 30 + u64::from(t - OPEN) / 60_000_000_000;
    let (day, minute) = (wall / 1_440 + 1, wall % 1_440);
    let (hour, minute) = (minute / 60, minute % 60);
    format!("day {day} {hour:02}:{minute:02}")
}

/// Everything this graph is fed from outside it, replayed from rows here.
/// Live, each field comes from its real source instead — the touches off a
/// market-data feed, the desireds from the desk — and the wiring in `main`
/// does not change. The venue's day only drives the test venue: a live
/// venue states its own, on `session.trading`.
struct Inputs {
    /// The market, per name.
    touches: Stream<Burst<Touch<Stock>>>,
    /// The venue's trading state, whenever it changes.
    venue_states: Stream<TradingState>,
    /// The desk's decisions, at the instants it made them.
    desired: Stream<Burst<Desired<Stock>>>,
}

impl Inputs {
    /// The inputs as canned rows, replayed at their instants — which is why
    /// `main` runs under `RunMode::HistoricalFrom`. A live source is a
    /// sibling constructor beside this one (`Inputs::live(&g, feed)`, say)
    /// returning the same streams, run under `RunMode::RealTime`; the
    /// wiring in `main` reads only these fields, so it does not change.
    fn replayed(g: &GraphBuilder) -> Self {
        let level = |price| Level::new(price, qty!(100));
        // A two-way quote on `stock` decided at `t`, as a replay row.
        let two_way = |t, stock, bid, ask| (Desired::two_way(stock, t, level(bid), level(ask)), t);

        let touches = g.replay(vec![
            (Touch::new(Stock::Aapl, px!(190.00), px!(190.05)), at(0)),
            (Touch::new(Stock::Msft, px!(421.00), px!(421.10)), at(0)),
        ]);

        // The venue's day: a volatility halt at 10:00, open again at 10:05,
        // the close at 16:00, and the next day's open and close.
        let venue_states = g
            .replay(vec![
                (TradingState::Halted, at(30)),
                (TradingState::Open, at(35)),
                (TradingState::Closed, at(390)),
                (TradingState::Open, at(1_440)),
                (TradingState::Closed, at(1_830)),
            ])
            .filter_map(|states: &Burst<TradingState>| states.last().copied());

        // The desk's decisions.
        let desired = g.replay(vec![
            // 09:31: quote both. This exchange has no post-only; the quotes
            // rest as day limits.
            two_way(at(1), Stock::Aapl, px!(189.95), px!(190.10)),
            two_way(at(1), Stock::Msft, px!(420.95), px!(421.15)),
            // 10:01, halted: re-price AAPL and pull MSFT. The withdrawal
            // goes; the re-price waits for the open.
            two_way(at(31), Stock::Aapl, px!(189.90), px!(190.15)),
            (Desired::nothing(Stock::Msft, at(31)), at(31)),
            // 16:01, closed: still wanted, and nothing is sent into the
            // closed book. The next open quotes it.
            two_way(at(391), Stock::Aapl, px!(189.90), px!(190.15)),
            // Day two, 09:31: take the MSFT offer. A cross is a limit,
            // immediate-or-cancel, capped at its price — and one-shot: it
            // fills as a taker and is not placed again, where a resting
            // bid priced through the ask would be wanted again the instant
            // it filled.
            (
                Desired::cross(Stock::Msft, at(1_441), Side::Bid, level(px!(421.20))),
                at(1_441),
            ),
        ]);
        Inputs {
            touches,
            venue_states,
            desired,
        }
    }
}

fn main() -> anyhow::Result<()> {
    let g = GraphBuilder::new();
    let inputs = Inputs::replayed(&g);

    // The venue, moving through its day, a millisecond of wire each way; the
    // OMS opened on it hears the venue's day as the session states it back,
    // a wire later, with nothing to wire for that.
    let venue = SimVenue::new(Profile::EXCHANGE, EPOCH, &inputs.touches)
        .with_trading(&inputs.venue_states)
        .with_latency(LATENCY);
    let oms = g.oms(CONFIG, EPOCH).venue(&venue);
    let session = oms.session();
    let (reports, trading) = (session.reports, session.trading);
    let (requests, _pacing) = oms.desired(&inputs.desired).build();

    // Printed as the OMS hears it, so each state lands beside what it
    // holds or lets go.
    trading
        .map(|state| Some(*state))
        .print_items_with(|t| format!("{} trading", clock(t)));
    requests.print_items_with(|t| format!("{} sent", clock(t)));
    reports.print_items_with(|t| format!("{} heard", clock(t)));

    let mut runner = g.build();
    runner.run(
        RunMode::HistoricalFrom(OPEN),
        RunFor::Duration(Duration::from_secs(1_832 * 60)),
    )?;
    Ok(())
}
