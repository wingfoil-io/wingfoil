#![cfg(feature = "execution-testing")]

//! The OMS against a FIX-shaped venue, through `ReplaceChain`, with no change
//! to the OMS.

use std::time::Duration;

use wingfoil::NanoTime;
use wingfoil::adapters::execution::edge::TradingState;
use wingfoil::adapters::execution::edge::{Report, Request};
use wingfoil::adapters::execution::fix::{Message, ReplaceChain};
use wingfoil::adapters::execution::oms::{Config, Desired, Lifetime, Oms, Passive, Slot};
use wingfoil::adapters::execution::order::Epoch;
use wingfoil::adapters::execution::rate_limit::{OrderRate, Terms};
use wingfoil::adapters::execution::testing::{FixVenue, Profile};
use wingfoil::adapters::market::{Level, Px, Qty, Side};

/// An index future and an equity.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash, PartialOrd, Ord)]
enum Contract {
    #[default]
    Es,
    Aapl,
}

const RATE: OrderRate = match OrderRate::new(
    Terms {
        rate: 100,
        burst: 100,
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

fn level(price: &str, size: &str) -> Level {
    Level::new(px(price), Qty::parse(size).unwrap())
}

fn at(ms: u64) -> NanoTime {
    NanoTime::from(1_000_000_000_000 + ms * 1_000_000)
}

fn two_way(contract: Contract, bid: &str, ask: &str, as_of: NanoTime) -> Desired<Contract> {
    Desired::two_way(contract, as_of, level(bid, "2"), level(ask, "2"))
}

/// The OMS, the chain and the venue, wired in a loop.
struct Rig {
    oms: Oms<Contract>,
    chain: ReplaceChain<Contract>,
    venue: FixVenue<Contract>,
}

impl Rig {
    fn new(profile: Profile, chain_mass_cancel: bool) -> Rig {
        let mut venue = FixVenue::new(profile);
        venue.touch(at(0), Contract::Es, Some(px("5000")), Some(px("5000.25")));
        venue.touch(
            at(0),
            Contract::Aapl,
            Some(px("190.00")),
            Some(px("190.05")),
        );
        Rig {
            oms: Oms::new(Config {
                max_desired_age: Duration::from_secs(60),
                min_requote: Px::ZERO,
                retake: Duration::from_secs(1),
                rate: RATE,
                passive: wingfoil::adapters::execution::oms::Passive::PostOnly,
                ratio: None,
                lifetime: wingfoil::adapters::execution::oms::Lifetime::GoodTillCancel,
            }),
            chain: ReplaceChain::new(chain_mass_cancel, Epoch::ZERO),
            venue,
        }
    }

    fn send(&mut self, now: NanoTime, requests: &[Request<Contract>]) -> Vec<Report<Contract>> {
        let (messages, mut reports) = self.chain.send(now, requests);
        let executions = self.venue.send(now, &messages);
        reports.extend(self.chain.receive(now, &executions));
        self.oms.apply(&reports);
        reports
    }

    fn decide(&mut self, now: NanoTime, desired: &[Desired<Contract>]) -> Vec<Report<Contract>> {
        let requests = self.oms.diff(now, desired);
        self.send(now, &requests)
    }

    fn touch(&mut self, now: NanoTime, contract: Contract, bid: &str, ask: &str) {
        let executions = self
            .venue
            .touch(now, contract, Some(px(bid)), Some(px(ask)));
        let reports = self.chain.receive(now, &executions);
        self.oms.apply(&reports);
    }

    fn cancel_all(&mut self, now: NanoTime) -> Vec<Report<Contract>> {
        let request = self.oms.cancel_all(now).expect("a token");
        self.send(now, &[request])
    }

    fn working(&self, contract: Contract, side: Side) -> Option<Px> {
        match self.oms.slot(&contract, side) {
            Slot::Working(resting) => Some(resting.price),
            _ => None,
        }
    }
}

/// Post-only stays until step 4, so this venue has it; it has no mass
/// cancel.
const POST_ONLY_NO_MASS_CANCEL: Profile = Profile {
    post_only: true,
    mass_cancel: false,
};

#[test]
fn a_replace_renames_the_order_at_the_venue_and_not_in_the_oms() {
    let mut rig = Rig::new(POST_ONLY_NO_MASS_CANCEL, false);
    rig.decide(at(1), &[two_way(Contract::Es, "4999.75", "5000.50", at(1))]);
    assert_eq!(rig.working(Contract::Es, Side::Bid), Some(px("4999.75")));
    let Slot::Working(bid) = rig.oms.slot(&Contract::Es, Side::Bid) else {
        panic!()
    };
    let before = rig.chain.current(bid.id).unwrap();

    rig.decide(at(2), &[two_way(Contract::Es, "4999.50", "5000.75", at(2))]);
    assert_eq!(rig.working(Contract::Es, Side::Bid), Some(px("4999.50")));
    let after = rig.chain.current(bid.id).unwrap();
    assert_ne!(before, after, "the venue renamed it");
    assert!(rig.venue.working(before).is_none());
    assert_eq!(rig.venue.working(after).unwrap().price, px("4999.50"));
    let Slot::Working(still) = rig.oms.slot(&Contract::Es, Side::Bid) else {
        panic!()
    };
    assert_eq!(still.id, bid.id, "the OMS's id never changed");
}

/// A fill on the renamed order reaches the OMS's slot.
#[test]
fn a_fill_after_a_replace_is_the_oms_order() {
    let mut rig = Rig::new(POST_ONLY_NO_MASS_CANCEL, false);
    rig.decide(at(1), &[two_way(Contract::Es, "4999.75", "5000.50", at(1))]);
    rig.decide(at(2), &[two_way(Contract::Es, "4999.50", "5000.50", at(2))]);
    rig.touch(at(3), Contract::Es, "4999.25", "4999.50");
    assert!(matches!(rig.oms.slot(&Contract::Es, Side::Bid), Slot::Idle));
    assert!(rig.working(Contract::Es, Side::Ask).is_some());
    assert_eq!(rig.chain.live(), 1);
    assert_eq!(rig.oms.unrouted(), 0);
}

/// An amend says what shows; a FIX replace's `OrderQty` says the total, and
/// the venue rests it less what has filled. After half the bid fills and the
/// strategy wants the same level again, the OMS amends back up to the whole
/// size, the chain states that as filled plus shown, and what the venue then
/// rests is what the OMS believes is showing.
#[test]
fn an_amend_after_a_partial_fill_rests_what_the_oms_believes() {
    let mut rig = Rig::new(POST_ONLY_NO_MASS_CANCEL, false);
    rig.decide(at(1), &[two_way(Contract::Es, "4999.75", "5000.50", at(1))]);
    let Slot::Working(bid) = rig.oms.slot(&Contract::Es, Side::Bid) else {
        panic!()
    };
    let at_venue = rig.chain.current(bid.id).unwrap();

    let filled = Qty::parse("0.5").unwrap();
    let print = rig
        .venue
        .execute(at(2), at_venue, filled)
        .expect("part of a resting order");
    let reports = rig.chain.receive(at(2), &[print]);
    rig.oms.apply(&reports);
    let Slot::Working(partial) = rig.oms.slot(&Contract::Es, Side::Bid) else {
        panic!("a partial does not retire the slot")
    };
    assert_eq!(partial.remaining, Qty::parse("1.5").unwrap());

    // The same decision again: the OMS amends the bid back up to 2.
    let requests = rig
        .oms
        .diff(at(3), &[two_way(Contract::Es, "4999.75", "5000.50", at(3))]);
    let [Request::Amend(amend)] = requests.as_slice() else {
        panic!("{requests:?}")
    };
    assert_eq!((amend.order, amend.qty), (bid.id, Qty::parse("2").unwrap()));

    let (messages, refused) = rig.chain.send(at(3), &requests);
    assert!(refused.is_empty());
    let [Message::Replace { qty: order_qty, .. }] = messages.as_slice() else {
        panic!("{messages:?}")
    };
    assert_eq!(*order_qty, filled + amend.qty, "the total, on the wire");
    let executions = rig.venue.send(at(3), &messages);
    let reports = rig.chain.receive(at(3), &executions);
    rig.oms.apply(&reports);

    let Slot::Working(resting) = rig.oms.slot(&Contract::Es, Side::Bid) else {
        panic!("the replace was taken: {executions:?}")
    };
    let renamed = rig.chain.current(bid.id).unwrap();
    assert_ne!(renamed, at_venue);
    let shown = rig.venue.working(renamed).unwrap();
    assert_eq!(shown.remaining, resting.remaining);
    assert_eq!(shown.remaining, amend.qty);
    assert_eq!(shown.filled, filled);
}

/// Step 3: no mass cancel, so cancel-all is one cancel per working order,
/// and every slot retires.
#[test]
fn cancel_all_without_a_mass_cancel_pulls_each_order() {
    let mut rig = Rig::new(POST_ONLY_NO_MASS_CANCEL, false);
    rig.decide(
        at(1),
        &[
            two_way(Contract::Es, "4999.75", "5000.50", at(1)),
            two_way(Contract::Aapl, "189.95", "190.10", at(1)),
        ],
    );
    assert_eq!(rig.venue.resting().count(), 4);
    let reports = rig.cancel_all(at(2));
    assert_eq!(
        reports
            .iter()
            .filter(|r| matches!(r, Report::Cancelled(_)))
            .count(),
        4
    );
    assert_eq!(rig.venue.resting().count(), 0);
    for contract in [Contract::Es, Contract::Aapl] {
        for side in [Side::Bid, Side::Ask] {
            assert!(matches!(rig.oms.slot(&contract, side), Slot::Idle));
        }
    }
    assert_eq!(rig.chain.live(), 0);
}

/// Where the venue has a mass cancel, the chain sends one, and its per-order
/// reports retire the slots.
#[test]
fn a_mass_cancel_is_sent_where_the_venue_has_one() {
    let mut rig = Rig::new(
        Profile {
            post_only: true,
            mass_cancel: true,
        },
        true,
    );
    rig.decide(at(1), &[two_way(Contract::Es, "4999.75", "5000.50", at(1))]);
    rig.cancel_all(at(2));
    assert_eq!(rig.venue.resting().count(), 0);
    assert!(matches!(rig.oms.slot(&Contract::Es, Side::Ask), Slot::Idle));
    assert_eq!(rig.chain.live(), 0);
}

/// A chain told the venue has a mass cancel, against one that has none: the
/// refusal names no order, the OMS reads it as a refused cancel-all, and its
/// next diff pulls each order singly.
#[test]
fn a_refused_mass_cancel_falls_back_to_single_cancels() {
    let mut rig = Rig::new(POST_ONLY_NO_MASS_CANCEL, true);
    rig.decide(at(1), &[two_way(Contract::Es, "4999.75", "5000.50", at(1))]);
    rig.cancel_all(at(2));
    assert_eq!(rig.oms.cancel_all_refused(), 1);
    assert_eq!(rig.venue.resting().count(), 2);
    rig.decide(at(3), &[]);
    assert_eq!(rig.venue.resting().count(), 0);
    assert!(matches!(rig.oms.slot(&Contract::Es, Side::Bid), Slot::Idle));
}

/// An amend for an order the venue already filled is answered at once as
/// unknown, so the slot never waits on a message that was not sent.
#[test]
fn a_request_for_a_gone_order_is_refused_without_being_sent() {
    let mut chain = ReplaceChain::<Contract>::new(false, Epoch::ZERO);
    let (messages, refused) = chain.send(
        at(1),
        &[Request::Cancel(
            wingfoil::adapters::execution::order::ClientOrderId(9),
        )],
    );
    assert!(messages.is_empty());
    assert!(matches!(refused.as_slice(), [Report::Reject(_)]));
}

/// an exchange with neither post-only nor mass cancel. Passive levels
/// rest as plain limits; a level priced through the touch fills as a taker,
/// which is why the guard is the strategy's.
#[test]
fn a_venue_without_post_only_rests_limits_and_fills_a_cross() {
    let mut rig = Rig::new(Profile::EXCHANGE, false);
    rig.oms = Oms::new(Config {
        passive: Passive::Limit,
        ..*rig.oms.config()
    });
    rig.decide(at(1), &[two_way(Contract::Es, "4999.75", "5000.50", at(1))]);
    assert_eq!(rig.working(Contract::Es, Side::Bid), Some(px("4999.75")));
    assert_eq!(rig.venue.resting().count(), 2);

    // Under post-only this venue refuses everything.
    let mut refused = Rig::new(Profile::EXCHANGE, false);
    refused.decide(at(1), &[two_way(Contract::Es, "4999.75", "5000.50", at(1))]);
    assert_eq!(refused.venue.resting().count(), 0);
    assert_eq!(refused.oms.rejected(), 2);

    // A bid priced through the ask — a strategy on a stale touch — trades.
    let reports = rig.decide(at(2), &[two_way(Contract::Aapl, "190.10", "190.20", at(2))]);
    assert!(reports.iter().any(|r| matches!(
        r,
        Report::Fill(fill) if fill.liquidity == wingfoil::adapters::execution::order::Liquidity::Taker
    )));
    assert!(matches!(
        rig.oms.slot(&Contract::Aapl, Side::Bid),
        Slot::Idle
    ));
}

/// a trading day at an exchange. Day orders go out at the open; a
/// halt holds a re-price but not a withdrawal; the close expires what rests
/// and nothing is sent into it; the next open quotes again.
#[test]
fn a_trading_day_open_halt_close_and_open_again() {
    let mut rig = Rig::new(Profile::EXCHANGE, false);
    rig.oms = Oms::new(Config {
        passive: Passive::Limit,
        lifetime: Lifetime::Day,
        ..*rig.oms.config()
    });
    let state = |rig: &mut Rig, now, state| {
        let executions = rig.venue.set_state(now, state);
        let reports = rig.chain.receive(now, &executions);
        rig.oms.apply(&reports);
        rig.oms.trading(state);
    };

    rig.decide(
        at(1),
        &[
            two_way(Contract::Es, "4999.75", "5000.50", at(1)),
            two_way(Contract::Aapl, "189.95", "190.10", at(1)),
        ],
    );
    assert_eq!(rig.venue.resting().count(), 4);
    assert!(
        rig.venue
            .resting()
            .all(|(_, order)| order.tif == wingfoil::adapters::execution::order::TimeInForce::Day)
    );

    // Halted: the re-price on ES waits; pulling AAPL does not.
    state(&mut rig, at(2), TradingState::Halted);
    rig.decide(
        at(3),
        &[
            two_way(Contract::Es, "4999.50", "5000.75", at(3)),
            Desired::nothing(Contract::Aapl, at(3)),
        ],
    );
    assert_eq!(rig.working(Contract::Es, Side::Bid), Some(px("4999.75")));
    assert!(matches!(
        rig.oms.slot(&Contract::Aapl, Side::Bid),
        Slot::Idle
    ));
    assert_eq!(rig.venue.resting().count(), 2);

    // Reopened: the held re-price goes.
    state(&mut rig, at(4), TradingState::Open);
    rig.decide(at(4), &[]);
    assert_eq!(rig.working(Contract::Es, Side::Bid), Some(px("4999.50")));

    // The close expires every day order, and the slots retire.
    state(&mut rig, at(5), TradingState::Closed);
    assert_eq!(rig.venue.resting().count(), 0);
    assert!(matches!(rig.oms.slot(&Contract::Es, Side::Bid), Slot::Idle));
    assert_eq!(rig.chain.live(), 0);
    let into_the_close = rig.decide(at(6), &[two_way(Contract::Es, "4999.50", "5000.75", at(6))]);
    assert!(into_the_close.is_empty(), "nothing sent into a closed book");
    assert_eq!(rig.venue.resting().count(), 0);

    // The next open quotes what is still wanted.
    state(&mut rig, at(7), TradingState::Open);
    rig.decide(at(7), &[]);
    assert_eq!(rig.venue.resting().count(), 2);
    assert_eq!(rig.oms.unrouted(), 0);
}
