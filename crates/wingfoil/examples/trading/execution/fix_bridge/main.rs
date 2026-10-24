//! The OMS in front of a FIX-shaped venue, unchanged, in a graph.
//!
//! `cargo run -p wingfoil --example execution_fix_bridge --features execution-testing`
//!
//! A FIX venue renames an order on every replace (`OrigClOrdID` →
//! `ClOrdID`) and may have no mass cancel. The OMS keeps one
//! `ClientOrderId` per order and says `CancelAll`; `fix::FixBridge` maps
//! between the two, so the diff never learns a venue id exists. The test
//! venue, `testing::SimVenue`, is a `FixVenue` behind that bridge as one
//! node: what comes back up the wire is the OMS's own ids, with the venue's
//! current `ClOrdID` carried on each ack as its `VenueId`.

use std::time::Duration;

use wingfoil::prelude::*;
use wingfoil::trading::execution::oms::{Config, Desired, OmsLoopOps};
use wingfoil::trading::execution::order::Epoch;
use wingfoil::trading::execution::rate_limit::{OrderRate, Terms};
use wingfoil::trading::execution::testing::{Profile, SimVenue, Touch};
use wingfoil::trading::market::{Level, Px, px, qty};
use wingfoil::{NanoTime, RunFor, RunMode};

/// The E-mini S&P future.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash, PartialOrd, Ord)]
struct Es;

const START: NanoTime = NanoTime::ZERO;

/// The latency to the venue, each way.
const LATENCY: Duration = Duration::from_millis(1);

/// Mints the OMS's order ids. A backtest has no earlier process whose ids
/// could collide, so the epoch is zero.
const EPOCH: Epoch = Epoch::ZERO;

const RATE: OrderRate = OrderRate::stated(Terms::per_second(50, 50), Terms::per_second(1, 1), 1.0);

const CONFIG: Config = Config::unmetered()
    .with_rate(RATE)
    .with_max_desired_age(Duration::from_secs(60));

/// Post-only, so the OMS's default passive kind is taken; no mass cancel.
const VENUE: Profile = Profile {
    post_only: true,
    mass_cancel: false,
};

fn at(ms: u64) -> NanoTime {
    START + Duration::from_millis(ms)
}

/// Everything this graph is fed from outside it, replayed from rows here.
/// Live, each field comes from its real source instead — the touches off a
/// market-data feed, the desireds from the strategy, the cancel-all from an
/// operator or a kill switch — and the wiring in `main` does not change.
struct Inputs {
    /// The market.
    touches: Stream<Burst<Touch<Es>>>,
    /// The strategy's decisions, at the instants it made them.
    desired: Stream<Burst<Desired<Es>>>,
    /// Pull everything.
    cancel_all: Stream<()>,
}

impl Inputs {
    /// The inputs as canned rows, replayed at their instants — which is why
    /// `main` runs under `RunMode::HistoricalFrom`. A live source is a
    /// sibling constructor beside this one (`Inputs::live(&g, feed)`, say)
    /// returning the same streams, run under `RunMode::RealTime`; the
    /// wiring in `main` reads only these fields, so it does not change.
    fn replayed(g: &GraphBuilder) -> Self {
        /// A two-way quote decided at `t`, as a replay row.
        fn two_way(t: NanoTime, bid: Px, ask: Px) -> (Desired<Es>, NanoTime) {
            let level = |price| Level::new(price, qty!(5));
            (Desired::two_way(Es, t, level(bid), level(ask)), t)
        }

        // The market; at 6ms it trades down through the bid and is back a
        // millisecond later.
        let touches = g.replay(vec![
            (Touch::new(Es, px!(5000.00), px!(5000.25)), at(0)),
            (Touch::new(Es, px!(4999.25), px!(4999.50)), at(6)),
            (Touch::new(Es, px!(5000.00), px!(5000.25)), at(7)),
        ]);

        // Quote; re-price once the acks are back, which the venue answers by
        // renaming both orders. The fill lands on the renamed order and
        // answers the bid, which is not placed again until a fresh decision.
        // Then cancel all, on a venue with no mass cancel.
        let desired = g.replay(vec![
            two_way(at(1), px!(4999.75), px!(5000.50)),
            two_way(at(4), px!(4999.50), px!(5000.75)),
        ]);
        let cancel_all = g.replay(vec![((), at(10))]).map(|_: &Burst<()>| ());
        Inputs {
            touches,
            desired,
            cancel_all,
        }
    }
}

fn main() -> anyhow::Result<()> {
    let g = GraphBuilder::new();
    let inputs = Inputs::replayed(&g);

    // The venue, a millisecond of wire each way, and the OMS opened on it.
    let venue = SimVenue::new(VENUE, EPOCH, &inputs.touches).with_latency(LATENCY);
    let oms = g.oms(CONFIG, EPOCH).venue(&venue);
    let reports = oms.session().reports;
    let (requests, _pacing) = oms
        .cancel_all(&inputs.cancel_all)
        .desired(&inputs.desired)
        .build();

    requests.print_items("sent");
    reports.print_items("heard");

    let mut runner = g.build();
    runner.run(
        RunMode::HistoricalFrom(START),
        RunFor::Duration(Duration::from_millis(15)),
    )?;
    Ok(())
}
