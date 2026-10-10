#![cfg(feature = "execution-testing")]

//! The OMS against a FIX-shaped venue, through `ReplaceChain`, with no change
//! to the OMS.

use std::time::Duration;

use wingfoil::NanoTime;
use wingfoil::adapters::execution::edge::TradingState;
use wingfoil::adapters::execution::edge::{Ack, Reject, RejectReason, Retired};
use wingfoil::adapters::execution::edge::{Report, Request};
use wingfoil::adapters::execution::exec_id::{ExecId, VenueId};
use wingfoil::adapters::execution::fix::{Message, ReplaceChain};
use wingfoil::adapters::execution::oms::OmsOps;
use wingfoil::adapters::execution::oms::{
    Config, Desired, Intent, Ladder, Lifetime, Oms, Passive, Slot,
};
use wingfoil::adapters::execution::order::Epoch;
use wingfoil::adapters::execution::order::{ClientOrderId, Fill, Liquidity, Order, TimeInForce};
use wingfoil::adapters::execution::rate_limit::{OrderRate, Terms};
use wingfoil::adapters::execution::testing::{FixVenue, Profile, SimVenue, Touch};
use wingfoil::adapters::execution::venue::Venue;
use wingfoil::adapters::market::{Level, Px, Qty, Side};
use wingfoil::prelude::*;
use wingfoil::{RunFor, RunMode};

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
    Desired {
        instrument: contract,
        bids: Ladder::one(level(bid, "2")),
        asks: Ladder::one(level(ask, "2")),
        as_of,
        intent: Intent::Rest,
        reduce_only: false,
    }
}

/// Post-only, good till cancelled, a decision believed for a minute.
fn config() -> Config {
    Config {
        max_desired_age: Duration::from_secs(60),
        min_requote: Px::ZERO,
        retake: Duration::from_secs(1),
        rate: RATE,
        passive: Passive::PostOnly,
        ratio: None,
        lifetime: Lifetime::GoodTillCancel,
    }
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
            oms: Oms::new(config()),
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

// The same stack as one graph: the OMS node, the caller's feedback cut on
// the request wire, and the test venue wired as a `Venue`.

fn ns(nanos: u64) -> NanoTime {
    NanoTime::new(nanos)
}

fn touch(bid: &str, ask: &str) -> Touch<Contract> {
    Touch {
        instrument: Contract::Es,
        bid: Some(px(bid)),
        ask: Some(px(ask)),
    }
}

/// What the loop sent and heard, each burst at the instant it ticked.
struct Loop {
    requests: Vec<(NanoTime, Vec<Request<Contract>>)>,
    reports: Vec<(NanoTime, Vec<Report<Contract>>)>,
}

fn flat<T: Copy + Default>(rows: Vec<(NanoTime, Burst<T>)>) -> Vec<(NanoTime, Vec<T>)> {
    rows.into_iter()
        .map(|(at, burst)| (at, burst.to_vec()))
        .collect()
}

/// What one closed-loop run is fed, each row at its engine instant.
struct Script {
    config: Config,
    touches: Vec<(Touch<Contract>, u64)>,
    desired: Vec<(Desired<Contract>, u64)>,
    cancel_all: Vec<u64>,
    trading: Vec<(TradingState, u64)>,
}

impl Script {
    /// The touch at 1, one two-way decision at 10, and nothing else.
    fn quote() -> Script {
        Script {
            config: config(),
            touches: vec![(touch("5000", "5000.25"), 1)],
            desired: vec![(two_way(Contract::Es, "4999.75", "5000.50", ns(10)), 10)],
            cancel_all: Vec::new(),
            trading: Vec::new(),
        }
    }
}

fn rows<T: Clone + Default + 'static>(g: &GraphBuilder, rows: Vec<(T, u64)>) -> Stream<Burst<T>> {
    g.replay_results(rows.into_iter().map(|(row, at)| Ok((row, ns(at)))))
}

/// One graph: the replayed decisions and a cancel-all into the OMS; its
/// requests through a feedback cut into the venue, which the touch and the
/// trading state also drive; the venue's reports and trading state back
/// into the OMS.
fn closed_loop(script: Script) -> Loop {
    let g = GraphBuilder::new();
    let touches = rows(&g, script.touches);
    let desired = rows(&g, script.desired);
    let pulls = script.cancel_all.into_iter().map(|at| ((), at)).collect();
    let cancel_all = rows(&g, pulls).map(|_: &Burst<()>| ());
    let trading =
        rows(&g, script.trading).filter_map(|states: &Burst<TradingState>| states.last().copied());
    let sweep = g.never();

    // The cut: what the OMS sends at one instant lands at the venue the next.
    let (landed, cut) = g.feedback::<Burst<Request<Contract>>>();
    let session = SimVenue::new(POST_ONLY_NO_MASS_CANCEL, Epoch::ZERO, &touches)
        .with_trading(&trading)
        .wire(&landed);
    let (requests, _pacing) = desired.oms(
        script.config,
        Epoch::ZERO,
        &session.reports,
        &session.trading,
        &cancel_all,
        &sweep,
    );
    let requests = requests.feedback(&cut).with_time().accumulate();
    let reports = session.reports.with_time().accumulate();

    let mut runner = g.build();
    runner
        .run(RunMode::HistoricalFrom(NanoTime::ZERO), RunFor::Forever)
        .unwrap();
    Loop {
        requests: flat(runner.value(&requests)),
        reports: flat(runner.value(&reports)),
    }
}

fn quote(id: u64, side: Side, price: &str, at: u64) -> Request<Contract> {
    Request::Place(Order {
        created: ns(at),
        ..Order::post_only(
            ClientOrderId(id),
            Contract::Es,
            side,
            Qty::parse("2").unwrap(),
            px(price),
        )
    })
}

fn acked(id: u64, price: &str, at: u64) -> Report<Contract> {
    Report::Ack(Ack {
        order: ClientOrderId(id),
        venue_id: VenueId::new(&id.to_string()).unwrap(),
        venue_time: Some(ns(at)),
        price: Some(px(price)),
        remaining: Some(Qty::parse("2").unwrap()),
        recv_time: ns(at),
    })
}

/// The whole loop in one graph. The decision at 10 places both sides; the
/// cut lands them at the venue at 11, which acks them there. The touch at
/// 20 comes down through the bid, which fills as a maker; the OMS, still
/// wanting that bid, places it again at once, and at 21 the venue refuses
/// it — at 4999.75 it would now take the ask. The cancel-all at 30 is
/// pulled at 31 as one cancel of the ask, since this venue has no mass
/// cancel, and the refused bid is not sent again.
#[test]
fn the_oms_against_the_test_venue_in_one_graph() {
    let mut script = Script::quote();
    script.touches.push((touch("4999.50", "4999.75"), 20));
    script.cancel_all.push(30);
    let run = closed_loop(script);
    let two = Qty::parse("2").unwrap();
    assert_eq!(
        run.requests,
        [
            (
                ns(10),
                vec![
                    quote(1, Side::Bid, "4999.75", 10),
                    quote(2, Side::Ask, "5000.50", 10)
                ]
            ),
            (ns(20), vec![quote(3, Side::Bid, "4999.75", 20)]),
            (ns(30), vec![Request::CancelAll]),
        ]
    );
    assert_eq!(
        run.reports,
        [
            (
                ns(11),
                vec![acked(1, "4999.75", 11), acked(2, "5000.50", 11)]
            ),
            (
                ns(20),
                vec![Report::Fill(Fill {
                    order: ClientOrderId(1),
                    exec_id: ExecId::new("T1").unwrap(),
                    instrument: Contract::Es,
                    side: Side::Bid,
                    qty: two,
                    filled: two,
                    remaining: Qty::ZERO,
                    price: px("4999.75"),
                    fee: Qty::ZERO,
                    liquidity: Liquidity::Maker,
                    venue_time: Some(ns(20)),
                    recv_time: ns(20),
                })]
            ),
            (
                ns(21),
                vec![Report::Reject(Reject {
                    order: Some(ClientOrderId(3)),
                    reason: RejectReason::PostOnlyWouldCross,
                    venue_time: Some(ns(21)),
                    recv_time: ns(21),
                })]
            ),
            (
                ns(31),
                vec![Report::Cancelled(Retired {
                    order: ClientOrderId(2),
                    remaining: two,
                    venue_time: Some(ns(31)),
                    recv_time: ns(31),
                })]
            ),
        ]
    );
}

/// The venue's trading state reaches the OMS through the session. Day
/// orders placed at 10 and acked at 11 expire in the instant the venue
/// closes, and nothing is sent into the closed book though the decision
/// still wants both sides.
#[test]
fn a_close_in_the_graph_expires_day_orders_and_holds_the_quotes() {
    let mut script = Script::quote();
    script.config.lifetime = Lifetime::Day;
    script.trading.push((TradingState::Closed, 20));
    let run = closed_loop(script);
    let day = |request: Request<Contract>| match request {
        Request::Place(order) => Request::Place(Order {
            tif: TimeInForce::Day,
            ..order
        }),
        other => other,
    };
    assert_eq!(
        run.requests,
        [(
            ns(10),
            vec![
                day(quote(1, Side::Bid, "4999.75", 10)),
                day(quote(2, Side::Ask, "5000.50", 10))
            ]
        )]
    );
    let expired = |id: u64| {
        Report::Expired(Retired {
            order: ClientOrderId(id),
            remaining: Qty::parse("2").unwrap(),
            venue_time: Some(ns(20)),
            recv_time: ns(20),
        })
    };
    assert_eq!(
        run.reports,
        [
            (
                ns(11),
                vec![acked(1, "4999.75", 11), acked(2, "5000.50", 11)]
            ),
            (ns(20), vec![expired(1), expired(2)]),
        ]
    );
}
