//! A venue's message-to-trade ratio.
//!
//! `cargo run -p wingfoil --example execution_message_ratio --features execution`
//!
//! Beside its per-second limits, an exchange bounds how many order messages
//! a session sends per execution. `Config::ratio` makes the OMS spend it:
//! places and amends wait when it is used up, a fill buys more, and a
//! cancel is counted but never held — a kill switch must always be able to
//! pull its quotes.

use std::time::Duration;

use wingfoil::NanoTime;
use wingfoil::adapters::execution::edge::{Ack, Report, Request};
use wingfoil::adapters::execution::exec_id::ExecId;
use wingfoil::adapters::execution::oms::{
    Config, Desired, Intent, Ladder, Lifetime, Oms, Passive, Slot,
};
use wingfoil::adapters::execution::order::{Fill, Liquidity};
use wingfoil::adapters::execution::rate_limit::{OrderRate, Ratio, Terms};
use wingfoil::adapters::market::{Level, Px, Qty, Side};

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash, PartialOrd, Ord)]
struct Bund;

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

fn at(ms: u64) -> NanoTime {
    NanoTime::from(1_000_000_000_000 + ms * 1_000_000)
}

fn bid(price: &str, as_of: NanoTime) -> Desired<Bund> {
    Desired {
        instrument: Bund,
        bids: Ladder::one(Level::new(
            Px::parse(price).unwrap(),
            Qty::parse("10").unwrap(),
        )),
        asks: Ladder::none(),
        as_of,
        intent: Intent::Rest,
        reduce_only: false,
    }
}

fn ack_all(oms: &mut Oms<Bund>, sent: &[Request<Bund>]) {
    let acks: Vec<Report<Bund>> = sent
        .iter()
        .filter_map(Request::order)
        .map(|order| {
            Report::Ack(Ack {
                order,
                ..Ack::default()
            })
        })
        .collect();
    oms.apply(&acks);
}

fn main() {
    // Four messages free, then three per fill.
    let mut oms = Oms::<Bund>::new(Config {
        max_desired_age: Duration::from_secs(60),
        min_requote: Px::ZERO,
        retake: Duration::from_secs(1),
        rate: RATE,
        passive: Passive::PostOnly,
        ratio: Some(Ratio {
            messages_per_fill: 3,
            free: 4,
        }),
        lifetime: Lifetime::GoodTillCancel,
    });

    // A quoter chasing a book it never trades with: one place, then an
    // amend a tick — until the ratio says no.
    let mut prices = ["131.00", "131.01", "131.02", "131.03", "131.04", "131.05"].into_iter();
    for tick in 0..6 {
        let now = at(tick);
        let sent = oms.diff(now, &[bid(prices.next().unwrap(), now)]);
        ack_all(&mut oms, &sent);
        println!(
            "tick {tick}: sent {} — (messages, fills) = {:?}",
            sent.len(),
            oms.ratio().unwrap()
        );
    }
    let Slot::Working(resting) = oms.slot(&Bund, Side::Bid) else {
        panic!()
    };
    println!(
        "resting at {} while the strategy wants 131.05",
        resting.price
    );
    assert_ne!(resting.price, Px::parse("131.05").unwrap());

    // A fill buys three more messages, and the held re-price goes.
    oms.apply(&[Report::Fill(Fill {
        order: resting.id,
        exec_id: ExecId::new("x1").unwrap(),
        instrument: Bund,
        side: Side::Bid,
        qty: Qty::parse("4").unwrap(),
        filled: Qty::parse("4").unwrap(),
        remaining: Qty::parse("6").unwrap(),
        price: resting.price,
        fee: Qty::ZERO,
        liquidity: Liquidity::Maker,
        venue_time: None,
        recv_time: at(7),
    })]);
    let sent = oms.diff(at(8), &[]);
    ack_all(&mut oms, &sent);
    println!(
        "after a fill: sent {} — {:?}",
        sent.len(),
        oms.ratio().unwrap()
    );
    assert!(matches!(sent.as_slice(), [Request::Amend(_)]));

    // Use the rest up, then withdraw: the cancel is never held.
    for tick in 9..12 {
        let now = at(tick);
        let sent = oms.diff(
            now,
            &[bid(if tick % 2 == 0 { "131.06" } else { "131.07" }, now)],
        );
        ack_all(&mut oms, &sent);
    }
    let pulled = oms.diff(at(12), &[Desired::nothing(Bund, at(12))]);
    println!(
        "out of room, withdrawing: {pulled:?} — {:?}",
        oms.ratio().unwrap()
    );
    assert!(matches!(pulled.as_slice(), [Request::Cancel(_)]));
}
