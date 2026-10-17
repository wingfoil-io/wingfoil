//! The OMS on an instrument of your own, in a graph: desireds in, the
//! requests that close the gap out, the venue's answers back in.
//!
//! `cargo run -p wingfoil --example execution_oms_quote --features execution-testing`
//!
//! A market maker quoting equities. The strategy's decisions are replayed
//! at engine instants into the OMS node, opened on the test venue
//! (`OmsLoopOps`); what it sends goes down a millisecond of wire to the
//! venue, whose reports come back on the session. Nothing here advances a
//! clock by hand: the graph is run historically, and every line is printed
//! at the instant it happened.

use std::time::Duration;

use wingfoil::adapters::execution::oms::{Config, Desired, OmsLoopOps, Pacing};
use wingfoil::adapters::execution::order::Epoch;
use wingfoil::adapters::execution::rate_limit::{OrderRate, Terms};
use wingfoil::adapters::execution::testing::{Profile, SimVenue, Touch};
use wingfoil::adapters::market::{Level, Px, Qty, px, qty};
use wingfoil::prelude::*;
use wingfoil::{NanoTime, RunFor, RunMode};

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash, PartialOrd, Ord)]
enum Ticker {
    #[default]
    Aapl,
    Msft,
    Nvda,
}

impl Ticker {
    /// What each side of a quote shows.
    fn lot(self) -> Qty {
        match self {
            Ticker::Aapl => qty!(100),
            Ticker::Msft => qty!(50),
            Ticker::Nvda => qty!(200),
        }
    }
}

/// The run starts here; every instant below is milliseconds after it.
const START: NanoTime = NanoTime::ZERO;

/// The latency to the venue, each way.
const LATENCY: Duration = Duration::from_millis(1);

/// Mints the OMS's order ids. A backtest has no earlier process whose ids
/// could collide, so the epoch is zero.
const EPOCH: Epoch = Epoch::ZERO;

/// A venue's order-entry limits: 10 a second, burst 4, spent whole.
const RATE: OrderRate = OrderRate::stated(Terms::per_second(10, 4), Terms::per_second(1, 1), 1.0);

/// Post-only, good till cancelled, a decision believed for five seconds.
const CONFIG: Config = Config::unmetered()
    .with_rate(RATE)
    .with_max_desired_age(Duration::from_secs(5));

/// A venue with post-only and a mass cancel.
const VENUE: Profile = Profile {
    post_only: true,
    mass_cancel: true,
};

fn at(ms: u64) -> NanoTime {
    START + Duration::from_millis(ms)
}

/// Everything this graph is fed from outside it, replayed from rows here.
/// Live, each field comes from its real source instead — the touches off a
/// market-data feed, the desireds from the strategy — and the wiring in
/// `main` does not change.
struct Inputs {
    /// The market, per name.
    touches: Stream<Burst<Touch<Ticker>>>,
    /// The strategy's decisions, at the instants it made them.
    desired: Stream<Burst<Desired<Ticker>>>,
}

impl Inputs {
    /// The inputs as canned rows, replayed at their instants — which is why
    /// `main` runs under `RunMode::HistoricalFrom`. A live source is a
    /// sibling constructor beside this one (`Inputs::live(&g, feed)`, say)
    /// returning the same streams, run under `RunMode::RealTime`; the
    /// wiring in `main` reads only these fields, so it does not change.
    fn replayed(g: &GraphBuilder) -> Self {
        /// A two-way quote on `ticker` decided at `t`, as a replay row.
        fn quote(t: NanoTime, ticker: Ticker, bid: Px, ask: Px) -> (Desired<Ticker>, NanoTime) {
            let lot = ticker.lot();
            let desired = Desired::two_way(ticker, t, Level::new(bid, lot), Level::new(ask, lot));
            (desired, t)
        }

        // The market: one touch per name, stated before anything is quoted.
        let touches = g.replay(vec![
            (Touch::new(Ticker::Aapl, px!(189.54), px!(189.56)), at(0)),
            (Touch::new(Ticker::Msft, px!(421.00), px!(421.02)), at(0)),
            (Touch::new(Ticker::Nvda, px!(120.02), px!(120.03)), at(0)),
        ]);

        // The strategy's decisions, replayed at the instants it made them.
        let desired = g.replay(vec![
            // Nothing working: two two-ways are four post-only places —
            // exactly the burst the venue allows.
            quote(at(0), Ticker::Aapl, px!(189.50), px!(189.60)),
            quote(at(0), Ticker::Msft, px!(420.90), px!(421.10)),
            // The touch moves on AAPL only: one amend per side that changed,
            // and MSFT, which wants what is working, is left alone.
            quote(at(300), Ticker::Aapl, px!(189.55), px!(189.65)),
            quote(at(300), Ticker::Msft, px!(420.90), px!(421.10)),
            // Withdrawing is a decision: an empty desired cancels at once.
            (Desired::nothing(Ticker::Msft, at(600)), at(600)),
            // AAPL is then left alone past `max_desired_age`, and the OMS
            // withdraws it the instant it stales, without a decision — it
            // wakes itself for that; nothing here ticks a clock.
            //
            // Three two-ways at once are six places; four go now, two wait —
            // not queued, re-planned at the instant each token has refilled
            // (a tenth of a second apart at 10 a second).
            quote(at(10_000), Ticker::Aapl, px!(189.50), px!(189.60)),
            quote(at(10_000), Ticker::Msft, px!(420.90), px!(421.10)),
            quote(at(10_000), Ticker::Nvda, px!(120.00), px!(120.05)),
            // Then the strategy falls silent: five seconds on, all six
            // quotes stale at once, and their cancels are paced by the
            // same budget — four on the burst, the last two a token apart.
        ]);
        Inputs { touches, desired }
    }
}

fn main() -> anyhow::Result<()> {
    let g = GraphBuilder::new();
    let inputs = Inputs::replayed(&g);

    // The venue, a millisecond of wire each way, and the OMS opened on it:
    // the venue hears each request the instant after the OMS sent it, plus
    // the wire, and its reports come back up the same wire into the OMS.
    let venue = SimVenue::new(VENUE, EPOCH, &inputs.touches).with_latency(LATENCY);
    let oms = g.oms(CONFIG, EPOCH).venue(&venue);
    let reports = oms.session().reports;
    let (requests, pacing) = oms.desired(&inputs.desired).build();

    requests.print_items("sent");
    reports.print_items("heard");
    // The pacing, whenever it changes: what the budget held back.
    pacing
        .distinct()
        .with_time()
        .for_each(|(t, p): &(NanoTime, Pacing)| {
            println!("{} pacing {p:?}", (*t - START).pretty_exact());
            Ok(())
        });

    let mut runner = g.build();
    runner.run(
        RunMode::HistoricalFrom(START),
        RunFor::Duration(Duration::from_millis(15_500)),
    )?;
    Ok(())
}
