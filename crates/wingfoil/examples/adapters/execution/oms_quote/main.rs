//! The OMS on an instrument of your own: desireds in, the requests that
//! close the gap out.
//!
//! `cargo run -p wingfoil --example execution_oms_quote --features execution`
//!
//! A market maker quoting two equities. The strategy says what it wants
//! showing every tick; `Oms::diff` emits only what differs from what is
//! working, never a second request on a side with one in flight, and never
//! more than the venue's order-rate budget pays for.

use std::time::Duration;

use wingfoil::NanoTime;
use wingfoil::adapters::execution::edge::{Ack, Report, Request};
use wingfoil::adapters::execution::oms::{Config, Desired, Intent, Ladder, Oms, Slot};
use wingfoil::adapters::execution::rate_limit::{OrderRate, Terms};
use wingfoil::adapters::market::{Level, Px, Qty, Side};

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash, PartialOrd, Ord)]
enum Ticker {
    #[default]
    Aapl,
    Msft,
    Nvda,
}

const MS: u64 = 1_000_000;

fn at(ms: u64) -> NanoTime {
    NanoTime::from(1_000_000 * MS + ms * MS)
}

fn level(price: &str, size: &str) -> Level {
    Level::new(Px::parse(price).unwrap(), Qty::parse(size).unwrap())
}

fn two_way(ticker: Ticker, bid: Level, ask: Level, as_of: NanoTime) -> Desired<Ticker> {
    Desired {
        instrument: ticker,
        bids: Ladder::one(bid),
        asks: Ladder::one(ask),
        as_of,
        intent: Intent::Rest,
        reduce_only: false,
    }
}

/// A venue's order-entry limits: 10 a second, burst 4, spent whole.
const RATE: OrderRate = match OrderRate::new(
    Terms { rate: 10, burst: 4 },
    Terms { rate: 1, burst: 1 },
    1.0,
) {
    Ok(rate) => rate,
    Err(_) => panic!("valid"),
};

fn config() -> Config {
    Config {
        max_desired_age: Duration::from_secs(5),
        min_requote: Px::ZERO,
        retake: Duration::from_secs(1),
        rate: RATE,
        passive: wingfoil::adapters::execution::oms::Passive::PostOnly,
        ratio: None,
        lifetime: wingfoil::adapters::execution::oms::Lifetime::GoodTillCancel,
    }
}

fn show(label: &str, requests: &[Request<Ticker>]) {
    println!("{label}:");
    if requests.is_empty() {
        println!("  (nothing)");
    }
    for request in requests {
        match request {
            Request::Place(o) => println!(
                "  place  {:?} {:?} {:?} {} @ {}",
                o.id.0,
                o.instrument,
                o.side,
                o.qty,
                o.price.map_or("market".to_string(), |p| p.to_string())
            ),
            Request::Amend(a) => println!("  amend  {:?} → {} @ {}", a.order.0, a.qty, a.price),
            Request::Cancel(id) => println!("  cancel {:?}", id.0),
            Request::CancelAll => println!("  cancel all"),
        }
    }
}

fn acks(requests: &[Request<Ticker>], now: NanoTime) -> Vec<Report<Ticker>> {
    requests
        .iter()
        .filter_map(Request::order)
        .map(|order| {
            Report::Ack(Ack {
                order,
                recv_time: now,
                ..Ack::default()
            })
        })
        .collect()
}

fn main() {
    let mut oms = Oms::<Ticker>::new(config());

    // Nothing working: each two-way is two post-only places — four, which
    // is exactly the burst the venue allows.
    let wants = [
        two_way(
            Ticker::Aapl,
            level("189.50", "100"),
            level("189.60", "100"),
            at(0),
        ),
        two_way(
            Ticker::Msft,
            level("420.90", "50"),
            level("421.10", "50"),
            at(0),
        ),
    ];
    let out = oms.diff(at(0), &wants);
    show("t=0 two two-ways", &out);
    assert_eq!(out.len(), 4);

    // Asked again before the venue answers: every side has a request in
    // flight, so nothing is sent.
    let again = oms.diff(at(1), &wants);
    show("t=1 same again, nothing acked yet", &again);
    assert!(again.is_empty());

    oms.apply(&acks(&out, at(2)));
    assert!(matches!(
        oms.slot(&Ticker::Aapl, Side::Bid),
        Slot::Working(_)
    ));

    // The touch moves on AAPL only: one amend per side that changed, and
    // MSFT, which wants what is working, is left alone.
    let moved = [
        two_way(
            Ticker::Aapl,
            level("189.55", "100"),
            level("189.65", "100"),
            at(300),
        ),
        two_way(
            Ticker::Msft,
            level("420.90", "50"),
            level("421.10", "50"),
            at(300),
        ),
    ];
    let out = oms.diff(at(300), &moved);
    show("t=300 AAPL re-priced", &out);
    assert!(out.iter().all(|r| matches!(r, Request::Amend(_))));
    oms.apply(&acks(&out, at(301)));

    // Withdrawing is a decision: an empty desired cancels at once.
    let out = oms.diff(at(600), &[Desired::nothing(Ticker::Msft, at(600))]);
    show("t=600 MSFT withdrawn", &out);
    assert!(out.iter().all(|r| matches!(r, Request::Cancel(_))));

    // A strategy that stops deciding is withdrawn too, once its last
    // desired is older than `max_desired_age`.
    let out = oms.diff(at(6_000), &[]);
    show("t=6000 AAPL gone stale", &out);
    println!("pacing: {:?}", oms.pacing());

    // The budget: a burst of four. Three two-ways at once are six places;
    // four go now, two wait — not queued, re-planned on the next diff once a
    // token has refilled (a tenth of a second at 10 a second).
    let mut fresh = Oms::<Ticker>::new(config());
    let three = |as_of| {
        [
            two_way(
                Ticker::Aapl,
                level("189.50", "100"),
                level("189.60", "100"),
                as_of,
            ),
            two_way(
                Ticker::Msft,
                level("420.90", "50"),
                level("421.10", "50"),
                as_of,
            ),
            two_way(
                Ticker::Nvda,
                level("120.00", "200"),
                level("120.05", "200"),
                as_of,
            ),
        ]
    };
    let first = fresh.diff(at(10_000), &three(at(10_000)));
    show("t=10000 three two-ways against a burst of four", &first);
    println!("pacing: {:?}", fresh.pacing());
    assert_eq!(first.len(), 4);
    let later = fresh.diff(at(10_250), &three(at(10_250)));
    show("t=10250 the budget has refilled", &later);
    assert_eq!(later.len(), 2);
}
