//! A trading day at an exchange: no post-only, day orders, a halt and a
//! close.
//!
//! `cargo run -p wingfoil --example execution_exchange_day --features execution-testing`
//!
//! `Config::passive = Limit` rests quotes as plain limits where the venue has
//! no post-only; `Config::lifetime = Day` makes them day orders the close
//! expires; and the venue's `TradingState`, fed to `Oms::trading`, holds all
//! but cancels while the book is not open.

use std::time::Duration;

use wingfoil::NanoTime;
use wingfoil::adapters::execution::edge::Request;
use wingfoil::adapters::execution::edge::TradingState;
use wingfoil::adapters::execution::fix::ReplaceChain;
use wingfoil::adapters::execution::oms::{
    Config, Desired, Intent, Ladder, Lifetime, Oms, Passive, Slot,
};
use wingfoil::adapters::execution::rate_limit::{OrderRate, Terms};
use wingfoil::adapters::execution::testing::{FixVenue, Profile};
use wingfoil::adapters::market::{Level, Px, Qty, Side};

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash, PartialOrd, Ord)]
enum Stock {
    #[default]
    Aapl,
    Msft,
}

const RATE: OrderRate = match OrderRate::new(
    Terms {
        rate: 50,
        burst: 50,
    },
    Terms { rate: 1, burst: 1 },
    1.0,
) {
    Ok(rate) => rate,
    Err(_) => panic!("valid"),
};

fn px(s: &str) -> Px {
    Px::parse(s).unwrap()
}

/// Minutes after the 09:30 open.
fn at(minutes: u64) -> NanoTime {
    NanoTime::from(
        20_000 * 86_400 * 1_000_000_000 + (13 * 3600 + 30 * 60 + minutes * 60) * 1_000_000_000,
    )
}

fn two_way(stock: Stock, bid: &str, ask: &str, as_of: NanoTime) -> Desired<Stock> {
    Desired {
        instrument: stock,
        bids: Ladder::one(Level::new(px(bid), Qty::parse("100").unwrap())),
        asks: Ladder::one(Level::new(px(ask), Qty::parse("100").unwrap())),
        as_of,
        intent: Intent::Rest,
        reduce_only: false,
    }
}

struct Desk {
    oms: Oms<Stock>,
    chain: ReplaceChain<Stock>,
    venue: FixVenue<Stock>,
}

impl Desk {
    fn decide(&mut self, now: NanoTime, wanted: &[Desired<Stock>]) -> Vec<Request<Stock>> {
        let requests = self.oms.diff(now, wanted);
        let (messages, refused) = self.chain.send(now, &requests);
        let executions = self.venue.send(now, &messages);
        self.oms.apply(&refused);
        self.oms.apply(&self.chain.receive(now, &executions));
        requests.to_vec()
    }

    fn state(&mut self, now: NanoTime, state: TradingState) {
        let executions = self.venue.set_state(now, state);
        let reports = self.chain.receive(now, &executions);
        self.oms.apply(&reports);
        self.oms.trading(state);
        println!("[{state:?}] {} order(s) expired", reports.len());
    }

    fn show(&self, label: &str, sent: &[Request<Stock>]) {
        let names: Vec<&str> = sent
            .iter()
            .map(|r| match r {
                Request::Place(_) => "place",
                Request::Amend(_) => "amend",
                Request::Cancel(_) => "cancel",
                Request::CancelAll => "cancel-all",
            })
            .collect();
        println!(
            "{label}: sent {names:?}; resting at the venue: {}",
            self.venue.resting().count()
        );
    }
}

fn main() {
    let mut venue = FixVenue::new(Profile::EXCHANGE);
    venue.touch(at(0), Stock::Aapl, Some(px("190.00")), Some(px("190.05")));
    venue.touch(at(0), Stock::Msft, Some(px("421.00")), Some(px("421.10")));
    let mut desk = Desk {
        oms: Oms::new(Config {
            max_desired_age: Duration::from_secs(24 * 3600),
            min_requote: Px::ZERO,
            retake: Duration::from_secs(1),
            rate: RATE,
            passive: Passive::Limit,
            ratio: None,
            lifetime: Lifetime::Day,
        }),
        chain: ReplaceChain::new(false),
        venue,
    };

    // This exchange has no post-only; quotes rest as day limits.
    let sent = desk.decide(
        at(1),
        &[
            two_way(Stock::Aapl, "189.95", "190.10", at(1)),
            two_way(Stock::Msft, "420.95", "421.15", at(1)),
        ],
    );
    desk.show("09:31 quote both", &sent);
    let (_, order) = desk.venue.resting().next().unwrap();
    println!("  resting as {:?}, {:?}", order.kind, order.tif);

    // A volatility halt on the venue. A re-price waits; a withdrawal goes.
    desk.state(at(30), TradingState::Halted);
    let sent = desk.decide(
        at(31),
        &[
            two_way(Stock::Aapl, "189.90", "190.15", at(31)),
            Desired::nothing(Stock::Msft, at(31)),
        ],
    );
    desk.show("10:01 halted: re-price AAPL, pull MSFT", &sent);
    assert!(sent.iter().all(|r| matches!(r, Request::Cancel(_))));

    desk.state(at(35), TradingState::Open);
    let sent = desk.decide(at(35), &[]);
    desk.show("10:05 reopened: the held re-price goes", &sent);
    assert!(sent.iter().all(|r| matches!(r, Request::Amend(_))));

    // The close expires every day order; nothing is sent into it.
    desk.state(at(390), TradingState::Closed);
    assert!(matches!(desk.oms.slot(&Stock::Aapl, Side::Bid), Slot::Idle));
    let sent = desk.decide(
        at(391),
        &[two_way(Stock::Aapl, "189.90", "190.15", at(391))],
    );
    desk.show("16:01 closed: still wanted", &sent);
    assert!(sent.is_empty());

    // The next open quotes what is still wanted.
    desk.state(at(1_440), TradingState::Open);
    let sent = desk.decide(at(1_440), &[]);
    desk.show("next 09:30 open", &sent);
    assert_eq!(desk.venue.resting().count(), 2);

    // And the price guard is the strategy's: priced through the touch, a
    // limit trades as a taker.
    let before = desk.venue.resting().count();
    let sent = desk.decide(
        at(1_441),
        &[two_way(Stock::Msft, "421.20", "421.30", at(1_441))],
    );
    let rested = desk.venue.resting().count() - before;
    println!(
        "MSFT bid priced through the ask: {} placed, {} filled at once as a taker, {rested} rest",
        sent.len(),
        sent.len() - rested
    );
    assert_eq!(rested, 1);
}
