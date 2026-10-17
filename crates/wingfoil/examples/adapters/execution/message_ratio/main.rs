//! A venue's message-to-trade ratio, spent by the OMS node in a graph.
//!
//! `cargo run -p wingfoil --example execution_message_ratio --features execution-testing`
//!
//! Beside its per-second limits, an exchange bounds how many order messages
//! a session sends per execution. `Config::ratio` makes the OMS spend it:
//! places and amends wait when it is used up, a fill buys more, and a
//! cancel is counted but never held — a kill switch must always be able to
//! pull its quotes. The quoter's decisions are replayed into the OMS node,
//! opened on the test venue (`OmsLoopOps`), which answers them; the one fill
//! is the market trading down through the resting bid.

use std::time::Duration;

use wingfoil::prelude::*;
use wingfoil::trading::execution::oms::{Config, Desired, Ladder, OmsLoopOps};
use wingfoil::trading::execution::order::Epoch;
use wingfoil::trading::execution::rate_limit::{OrderRate, Ratio, Terms};
use wingfoil::trading::execution::testing::{Profile, SimVenue, Touch};
use wingfoil::trading::market::{Level, Px, Side, px, qty};
use wingfoil::{NanoTime, RunFor, RunMode};

/// The Bund future.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash, PartialOrd, Ord)]
struct Bund;

const START: NanoTime = NanoTime::ZERO;

/// The latency to the venue, each way.
const LATENCY: Duration = Duration::from_millis(1);

/// Mints the OMS's order ids. A backtest has no earlier process whose ids
/// could collide, so the epoch is zero.
const EPOCH: Epoch = Epoch::ZERO;

const RATE: OrderRate = OrderRate::stated(Terms::per_second(50, 50), Terms::per_second(1, 1), 1.0);

/// Four messages free, then three per fill.
const CONFIG: Config = Config::unmetered()
    .with_rate(RATE)
    .with_max_desired_age(Duration::from_secs(60))
    .with_ratio(Ratio {
        messages_per_fill: 3,
        free: 4,
    });

const VENUE: Profile = Profile {
    post_only: true,
    mass_cancel: true,
};

fn at(ms: u64) -> NanoTime {
    START + Duration::from_millis(ms)
}

/// Everything this graph is fed from outside it, replayed from rows here.
/// Live, each field comes from its real source instead — the touches off a
/// market-data feed, the desireds from the quoter — and the wiring in
/// `main` does not change.
struct Inputs {
    /// The market.
    touches: Stream<Burst<Touch<Bund>>>,
    /// The quoter's decisions, at the instants it made them.
    desired: Stream<Burst<Desired<Bund>>>,
}

impl Inputs {
    /// The inputs as canned rows, replayed at their instants — which is why
    /// `main` runs under `RunMode::HistoricalFrom`. A live source is a
    /// sibling constructor beside this one (`Inputs::live(&g, feed)`, say)
    /// returning the same streams, run under `RunMode::RealTime`; the
    /// wiring in `main` reads only these fields, so it does not change.
    fn replayed(g: &GraphBuilder) -> Self {
        /// Ten lots bid at `price`, nothing offered, decided at `t`, as a
        /// replay row.
        fn bid(t: NanoTime, price: Px) -> (Desired<Bund>, NanoTime) {
            let level = Level::new(price, qty!(10));
            (Desired::rest(Bund, t, Side::Bid, Ladder::one(level)), t)
        }

        // A wide market, so every bid below rests; at 20ms it trades down
        // through the resting bid and is back a millisecond later.
        let touches = g.replay(vec![
            (Touch::new(Bund, px!(130.00), px!(132.00)), at(0)),
            (Touch::new(Bund, px!(130.00), px!(131.03)), at(20)),
            (Touch::new(Bund, px!(130.00), px!(132.00)), at(21)),
        ]);

        // A quoter chasing a book it never trades with: one place, then an
        // amend every three milliseconds (the venue answers in two) — until the
        // ratio says no. The fill at 20ms buys three more messages; the bid is
        // placed again where it is wanted, and two more re-prices use the room
        // up. Then it withdraws.
        let desired = g.replay(vec![
            bid(at(0), px!(131.00)),
            bid(at(3), px!(131.01)),
            bid(at(6), px!(131.02)),
            bid(at(9), px!(131.03)),
            bid(at(12), px!(131.04)),
            bid(at(15), px!(131.05)),
            bid(at(24), px!(131.06)),
            bid(at(27), px!(131.07)),
            bid(at(30), px!(131.08)),
            (Desired::nothing(Bund, at(33)), at(33)),
        ]);
        Inputs { touches, desired }
    }
}

fn main() -> anyhow::Result<()> {
    let g = GraphBuilder::new();
    let inputs = Inputs::replayed(&g);

    // The venue, a millisecond of wire each way, and the OMS opened on it.
    // Read after each diff: the ratio's count — (messages, fills) — and
    // how many requests are waiting on it.
    let venue = SimVenue::new(VENUE, EPOCH, &inputs.touches).with_latency(LATENCY);
    let oms = g.oms(CONFIG, EPOCH).venue(&venue);
    let reports = oms.session().reports;
    let (requests, _pacing, standing) = oms
        .reading(|oms| (oms.ratio().unwrap_or_default(), oms.pacing().waiting))
        .desired(&inputs.desired)
        .build();

    // Each decision, with where the count stands once it has been diffed.
    inputs
        .desired
        .join_passive(&standing, |wanted, standing| (wanted.clone(), *standing))
        // The instant is the decision's own `as_of`, not a `with_time` node:
        // one hop fewer keeps this line ahead of the `sent` it causes.
        .for_each(|(wanted, ((messages, fills), waiting))| {
            let Some(t) = wanted.first().map(|d| d.as_of) else {
                return Ok(());
            };
            let want = wanted
                .iter()
                .map(|d| match d.bids.get(0) {
                    Some(level) => format!("bid {}", level.price),
                    None => "nothing".to_string(),
                })
                .collect::<Vec<_>>()
                .join(", ");
            let room = CONFIG.ratio.map_or(0, |r| {
                u64::from(r.free) + u64::from(r.messages_per_fill) * fills
            });
            let held = if *waiting > 0 { " — held" } else { "" };
            println!(
                "{} wants {want:<12} messages {messages} of {room}, fills {fills}{held}",
                (t - START).pretty_exact()
            );
            Ok(())
        });
    requests.print_items("sent");
    reports.print_items("heard");

    let mut runner = g.build();
    runner.run(
        RunMode::HistoricalFrom(START),
        RunFor::Duration(Duration::from_millis(40)),
    )?;
    Ok(())
}
