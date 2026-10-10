//! The OMS in front of a FIX-shaped venue, unchanged, through
//! `ReplaceChain`.
//!
//! `cargo run -p wingfoil --example execution_replace_chain --features execution-testing`
//!
//! A FIX venue renames an order on every replace (`OrigClOrdID` →
//! `ClOrdID`) and may have no mass cancel. The OMS keeps one
//! `ClientOrderId` per order and says `CancelAll`; the chain maps between
//! the two, so the diff never learns a venue id exists.

use std::time::Duration;

use wingfoil::NanoTime;
use wingfoil::adapters::execution::edge::{Report, Request};
use wingfoil::adapters::execution::fix::{ExecKind, ExecReport, Message, ReplaceChain};
use wingfoil::adapters::execution::oms::{
    Config, Desired, Intent, Ladder, Lifetime, Oms, Passive, Slot,
};
use wingfoil::adapters::execution::rate_limit::{OrderRate, Terms};
use wingfoil::adapters::execution::testing::{FixVenue, Profile};
use wingfoil::adapters::market::{Level, Px, Qty, Side};

/// The E-mini S&P future.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash, PartialOrd, Ord)]
struct Es;

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

fn at(ms: u64) -> NanoTime {
    NanoTime::from(1_000_000_000_000 + ms * 1_000_000)
}

fn two_way(bid: &str, ask: &str, as_of: NanoTime) -> Desired<Es> {
    Desired {
        instrument: Es,
        bids: Ladder::one(Level::new(px(bid), Qty::parse("5").unwrap())),
        asks: Ladder::one(Level::new(px(ask), Qty::parse("5").unwrap())),
        as_of,
        intent: Intent::Rest,
        reduce_only: false,
    }
}

fn show_out(messages: &[Message<Es>]) {
    for message in messages {
        match message {
            Message::New(o) => println!(
                "  → New     ClOrdID {:>2}  {:?} {} @ {}",
                o.cl_ord_id.0,
                o.side,
                o.qty,
                o.price.map_or("market".to_string(), |p| p.to_string())
            ),
            Message::Replace {
                cl_ord_id,
                orig,
                price,
                qty,
                ..
            } => {
                println!(
                    "  → Replace ClOrdID {:>2}  OrigClOrdID {}  → {qty} @ {price}",
                    cl_ord_id.0, orig.0
                )
            }
            Message::Cancel {
                cl_ord_id, orig, ..
            } => println!(
                "  → Cancel  ClOrdID {:>2}  OrigClOrdID {}",
                cl_ord_id.0, orig.0
            ),
            Message::MassCancel { cl_ord_id } => println!("  → MassCancel ClOrdID {}", cl_ord_id.0),
        }
    }
}

fn describe(kind: &ExecKind<Es>) -> String {
    match kind {
        ExecKind::New => "New".into(),
        ExecKind::Replaced { price, qty } => format!("Replaced → {qty} @ {price}"),
        ExecKind::Canceled { remaining } => format!("Canceled, {remaining} left"),
        ExecKind::Expired { remaining } => format!("Expired, {remaining} left"),
        ExecKind::Rejected(reason) => format!("Rejected {reason:?}"),
        ExecKind::Trade(t) => format!(
            "Trade {} @ {} ({:?}), {} left",
            t.qty, t.price, t.liquidity, t.remaining
        ),
    }
}

fn show_in(executions: &[ExecReport<Es>], reports: &[Report<Es>]) {
    for (execution, report) in executions.iter().zip(reports) {
        println!(
            "  ← {:<34} on ClOrdID {:>2}  ⇒  OMS order {}",
            describe(&execution.kind),
            execution.cl_ord_id.0,
            report
                .order()
                .map_or("-".to_string(), |id| id.sequence().to_string())
        );
    }
}

/// OMS → chain → venue → chain → OMS, for one burst of requests.
fn round_trip(
    now: NanoTime,
    oms: &mut Oms<Es>,
    chain: &mut ReplaceChain<Es>,
    venue: &mut FixVenue<Es>,
    requests: &[Request<Es>],
) {
    let (messages, refused) = chain.send(now, requests);
    show_out(&messages);
    let executions = venue.send(now, &messages);
    let reports = chain.receive(now, &executions);
    show_in(&executions, &reports);
    oms.apply(&refused);
    oms.apply(&reports);
}

fn main() {
    // Post-only, so the OMS's default passive kind is taken; no mass cancel.
    let mut venue = FixVenue::<Es>::new(Profile {
        post_only: true,
        mass_cancel: false,
    });
    venue.touch(at(0), Es, Some(px("5000.00")), Some(px("5000.25")));
    let mut chain = ReplaceChain::new(false);
    let mut oms = Oms::<Es>::new(Config {
        max_desired_age: Duration::from_secs(60),
        min_requote: Px::ZERO,
        retake: Duration::from_secs(1),
        rate: RATE,
        passive: Passive::PostOnly,
        ratio: None,
        lifetime: Lifetime::GoodTillCancel,
    });

    println!("quote 4999.75 / 5000.50:");
    let requests = oms.diff(at(1), &[two_way("4999.75", "5000.50", at(1))]);
    round_trip(at(1), &mut oms, &mut chain, &mut venue, &requests);
    let Slot::Working(bid) = oms.slot(&Es, Side::Bid) else {
        panic!()
    };
    let first = chain.current(bid.id).unwrap();

    println!("\nre-price to 4999.50 / 5000.75 — the venue renames both orders:");
    let requests = oms.diff(at(2), &[two_way("4999.50", "5000.75", at(2))]);
    round_trip(at(2), &mut oms, &mut chain, &mut venue, &requests);
    let renamed = chain.current(bid.id).unwrap();
    println!(
        "  OMS bid is still order {}; at the venue it went ClOrdID {} → {}",
        bid.id.sequence(),
        first.0,
        renamed.0
    );
    assert_ne!(first, renamed);

    println!("\nthe market trades down through the bid — a fill on the renamed order:");
    let executions = venue.touch(at(3), Es, Some(px("4999.25")), Some(px("4999.50")));
    let reports = chain.receive(at(3), &executions);
    show_in(&executions, &reports);
    oms.apply(&reports);
    assert!(matches!(oms.slot(&Es, Side::Bid), Slot::Idle));

    println!("\ncancel-all, on a venue with no mass cancel — one cancel per live order:");
    let request = oms.cancel_all(at(4)).expect("a token");
    round_trip(at(4), &mut oms, &mut chain, &mut venue, &[request]);
    assert_eq!(venue.resting().count(), 0);
    assert_eq!(chain.live(), 0);
    println!(
        "  nothing resting, nothing live in the chain, unrouted reports: {}",
        oms.unrouted()
    );
}
