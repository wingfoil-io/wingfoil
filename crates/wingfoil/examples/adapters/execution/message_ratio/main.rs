//! A venue's message-to-trade ratio, spent by the OMS node in a graph.
//!
//! `cargo run -p wingfoil --example execution_message_ratio --features execution-testing`
//!
//! Beside its per-second limits, an exchange bounds how many order messages
//! a session sends per execution. `Config::ratio` makes the OMS spend it:
//! places and amends wait when it is used up, a fill buys more, and a
//! cancel is counted but never held — a kill switch must always be able to
//! pull its quotes. The quoter's decisions are replayed into `OmsOps`; the
//! test venue behind the feedback cut answers them, and the one fill is the
//! market trading down through the resting bid.

use std::time::Duration;

use wingfoil::adapters::execution::edge::{Report, Request};
use wingfoil::adapters::execution::oms::{Config, Desired, Ladder, OmsOps};
use wingfoil::adapters::execution::order::Epoch;
use wingfoil::adapters::execution::rate_limit::{OrderRate, Ratio, Terms};
use wingfoil::adapters::execution::testing::{Profile, SimVenue, Touch};
use wingfoil::adapters::execution::venue::Venue;
use wingfoil::adapters::market::{Level, Px, Side, px, qty};
use wingfoil::prelude::*;
use wingfoil::{NanoTime, RunFor, RunMode};

/// The Bund future.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash, PartialOrd, Ord)]
struct Bund;

const START: NanoTime = NanoTime::ZERO;
const LATENCY: Duration = Duration::from_millis(1);

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

fn touch(bid: Px, ask: Px) -> Touch<Bund> {
    Touch {
        instrument: Bund,
        bid: Some(bid),
        ask: Some(ask),
    }
}

/// Ten lots bid at `price`, nothing offered.
fn bid(price: Px, as_of: NanoTime) -> Desired<Bund> {
    let level = Level::new(price, qty!(10));
    Desired::rest(Bund, as_of, Side::Bid, Ladder::one(level))
}

fn replay<T: Clone + Default + 'static>(
    g: &GraphBuilder,
    rows: Vec<(T, NanoTime)>,
) -> Stream<Burst<T>> {
    g.replay_results(rows.into_iter().map(Ok))
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

fn main() -> anyhow::Result<()> {
    let g = GraphBuilder::new();

    // A wide market, so every bid below rests; at 20ms it trades down
    // through the resting bid and is back a millisecond later.
    let touches = replay(
        &g,
        vec![
            (touch(px!(130.00), px!(132.00)), at(0)),
            (touch(px!(130.00), px!(131.03)), at(20)),
            (touch(px!(130.00), px!(132.00)), at(21)),
        ],
    );

    // A quoter chasing a book it never trades with: one place, then an
    // amend every three milliseconds (the venue answers in two) — until the
    // ratio says no. The fill at 20ms buys three more messages; the bid is
    // placed again where it is wanted, and two more re-prices use the room
    // up. Then it withdraws.
    let desired = replay(
        &g,
        vec![
            (bid(px!(131.00), at(0)), at(0)),
            (bid(px!(131.01), at(3)), at(3)),
            (bid(px!(131.02), at(6)), at(6)),
            (bid(px!(131.03), at(9)), at(9)),
            (bid(px!(131.04), at(12)), at(12)),
            (bid(px!(131.05), at(15)), at(15)),
            (bid(px!(131.06), at(24)), at(24)),
            (bid(px!(131.07), at(27)), at(27)),
            (bid(px!(131.08), at(30)), at(30)),
            (Desired::nothing(Bund, at(33)), at(33)),
        ],
    );

    let (requests, (reports, standing)) = g.feedback_loop(|sent_requests| {
        let session =
            SimVenue::new(VENUE, Epoch::ZERO, &touches).wire(&sent_requests.delay(LATENCY));
        let reports = session.reports.delay(LATENCY);
        // Read after each diff: the ratio's count — (messages, fills) — and
        // how many requests are waiting on it.
        let (requests, _pacing, standing) = desired
            .wire_oms(CONFIG, Epoch::ZERO, &reports)
            .reading(|oms| (oms.ratio().unwrap_or_default(), oms.pacing().waiting))
            .build();
        (requests, (reports, standing))
    });

    // Each decision, with where the count stands once it has been diffed.
    let _decisions = desired
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
    let _sent = requests.with_time().for_each(|(t, burst)| {
        for request in burst {
            println!("t={:>2}ms sent  {}", ms(*t), describe(request));
        }
        Ok(())
    });
    let _heard = reports.with_time().for_each(|(t, burst)| {
        for report in burst {
            println!("t={:>2}ms heard {}", ms(*t), heard(report));
        }
        Ok(())
    });

    let mut runner = g.build();
    runner.run(
        RunMode::HistoricalFrom(START),
        RunFor::Duration(Duration::from_millis(40)),
    )?;
    Ok(())
}
