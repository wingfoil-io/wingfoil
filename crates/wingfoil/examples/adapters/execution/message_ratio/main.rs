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

use wingfoil::adapters::execution::edge::{Report, Request};
use wingfoil::adapters::execution::oms::{Config, Desired, Ladder, OmsLoopOps};
use wingfoil::adapters::execution::order::Epoch;
use wingfoil::adapters::execution::rate_limit::{OrderRate, Ratio, Terms};
use wingfoil::adapters::execution::testing::{Profile, SimVenue, Touch};
use wingfoil::adapters::market::{Level, Px, Side, px, qty};
use wingfoil::prelude::*;
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

fn ms(t: NanoTime) -> u64 {
    u64::from(t - START) / 1_000_000
}

/// The market at `t`, as a replay row.
fn touch(t: NanoTime, bid: Px, ask: Px) -> (Touch<Bund>, NanoTime) {
    let touch = Touch {
        instrument: Bund,
        bid: Some(bid),
        ask: Some(ask),
    };
    (touch, t)
}

/// Ten lots bid at `price`, nothing offered, decided at `t`, as a replay row.
fn bid(t: NanoTime, price: Px) -> (Desired<Bund>, NanoTime) {
    let level = Level::new(price, qty!(10));
    (Desired::rest(Bund, t, Side::Bid, Ladder::one(level)), t)
}

/// Nothing wanted from `t`, as a replay row.
fn withdraw(t: NanoTime) -> (Desired<Bund>, NanoTime) {
    (Desired::nothing(Bund, t), t)
}

/// Print each item of each burst as it lands, after the instant and `what`.
fn print_bursts<T: Clone + Default + 'static>(
    stream: &Stream<Burst<T>>,
    what: &'static str,
    line: fn(&T) -> String,
) {
    stream.with_time().for_each(move |(t, burst)| {
        for item in burst {
            println!("t={:>2}ms {what:<5} {}", ms(*t), line(item));
        }
        Ok(())
    });
}

fn describe(request: &Request<Bund>) -> String {
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

fn heard(report: &Report<Bund>) -> String {
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
/// market-data feed, the desireds from the quoter — and the wiring in
/// `main` does not change.
struct Inputs {
    /// The market.
    touches: Stream<Burst<Touch<Bund>>>,
    /// The quoter's decisions, at the instants it made them.
    desired: Stream<Burst<Desired<Bund>>>,
}

impl Inputs {
    fn replayed(g: &GraphBuilder) -> Self {
        // A wide market, so every bid below rests; at 20ms it trades down
        // through the resting bid and is back a millisecond later.
        let touches = g.replay(vec![
            touch(at(0), px!(130.00), px!(132.00)),
            touch(at(20), px!(130.00), px!(131.03)),
            touch(at(21), px!(130.00), px!(132.00)),
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
            withdraw(at(33)),
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
        .with_time()
        .for_each(|(t, (wanted, ((messages, fills), waiting)))| {
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
                "t={:>2}ms wants {want:<12} messages {messages} of {room}, fills {fills}{held}",
                ms(*t)
            );
            Ok(())
        });
    print_bursts(&requests, "sent", describe);
    print_bursts(&reports, "heard", heard);

    let mut runner = g.build();
    runner.run(
        RunMode::HistoricalFrom(START),
        RunFor::Duration(Duration::from_millis(40)),
    )?;
    Ok(())
}
