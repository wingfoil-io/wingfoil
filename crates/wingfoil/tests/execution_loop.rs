#![cfg(feature = "execution-sim")]
//! **Gate P0 of Project Venue** (`docs/planning/proposals/trading-stack.md`):
//! the architectural claim under test is that "the simulated venue is just an
//! op", and this file is the test that establishes it.
//!
//! The claim, concretely: a strategy emitting [`Order`]s and consuming
//! [`Fill`]s closes into a loop — strategy → order → fill → position →
//! strategy — on the ordinary engine, with no kernel changes, and the loop is
//! deterministic under `RunMode::HistoricalFrom`. If that holds, everything
//! above it (risk, OMS, portfolio) is ordinary work; if it does not, the
//! project stops. So these assertions are load-bearing in a way most adapter
//! tests are not, and they pin **exact tick times** as well as values.
//!
//! Four things are pinned here beyond "it works", each because it is cheap to
//! assert while the loop is four ops long and expensive to retrofit once a
//! fill model sits on top of it:
//!
//! 1. **The carried shape of the feedback edge** is `Burst<Order>`
//!    ([`the_feedback_edge_carries_a_burst_of_orders`]) — §12's one genuinely
//!    open question on the order side, settled here.
//! 2. **Order-vs-book-update ordering within a cycle** (§4.2 decision 1): an
//!    order matches the book as of the *previous* instant, never the update it
//!    arrived alongside. [`an_order_cannot_fill_against_the_update_it_arrived_with`].
//! 3. **A fill lands strictly after its order** — the `feedback` `time + 1`
//!    floor, observed rather than assumed.
//! 4. **`TimeQueue` dedup cannot swallow an order**, because distinct
//!    [`ClOrdId`]s make two orders in one burst distinct values and a burst is
//!    one value anyway ([`two_near_identical_orders_in_one_burst_both_fill`]).
//!
//! The reference strategy is [`ReferenceQuoter`] — the simplest passive thing
//! that quotes and gets filled. It is a **test instrument**, not a
//! demonstration of demand, and it exists to give gate P2's honest fill model
//! a baseline to be measured against: re-run it unchanged against P2 and the
//! PnL must get materially worse for named reasons.

use std::sync::Arc;

use wingfoil::adapters::execution::sim::SimVenueOps;
use wingfoil::adapters::execution::{
    ClOrdId, Fill, Notional, Order, Position, PositionOps, TimeInForce,
};
use wingfoil::adapters::market::{
    BookDelta, BookSnapshot, BookUpdate, InstrumentId, Level, LevelChange, MarketBookOps,
    OrderBook, Px, Qty, Sequencing, Side,
};
use wingfoil::op;
use wingfoil::op::{Activation, Ctx, Op, Tick};
use wingfoil::prelude::*;
use wingfoil::{Burst, NanoTime, RunFor, RunMode, burst};

const HISTORICAL: RunMode = RunMode::HistoricalFrom(NanoTime::ZERO);

fn inst() -> InstrumentId {
    InstrumentId::new("test", "BTC-USD")
}

fn px(s: &str) -> Px {
    Px::parse(s).expect("test fixture price parses")
}

fn qty(s: &str) -> Qty {
    Qty::parse(s).expect("test fixture quantity parses")
}

/// A one-deep book at `(bid, ask)`, both a single unit.
fn snapshot(seq: u64, bid: &str, ask: &str) -> BookUpdate {
    BookUpdate::Snapshot(BookSnapshot {
        instrument: inst(),
        bids: vec![Level::new(px(bid), qty("1"))],
        asks: vec![Level::new(px(ask), qty("1"))],
        sequencing: Sequencing::Single(seq),
        venue_time: None,
        recv_time: NanoTime::ZERO,
    })
}

/// Move the one-deep book to `(bid, ask)` by deleting the old touch and
/// setting the new one.
fn move_touch(seq: u64, from: (&str, &str), to: (&str, &str)) -> BookUpdate {
    BookUpdate::Delta(BookDelta {
        instrument: inst(),
        changes: vec![
            LevelChange::new(Side::Bid, px(from.0), Qty::ZERO),
            LevelChange::new(Side::Ask, px(from.1), Qty::ZERO),
            LevelChange::new(Side::Bid, px(to.0), qty("1")),
            LevelChange::new(Side::Ask, px(to.1), qty("1")),
        ],
        sequencing: Sequencing::Single(seq),
        venue_time: None,
        recv_time: NanoTime::ZERO,
    })
}

/// The book feed every test in this file replays.
///
/// ```text
///   t=0    bid 100 / ask 101
///   t=10   bid  99 / ask 100   ← the ask reaches the resting buy
///   t=20   bid  99 / ask 101
///   t=30   bid 101 / ask 102   ← the bid reaches the resting sell
///   t=40   bid 100 / ask 101
/// ```
///
/// Each move is a full round trip for the quoter below: it buys the bid at
/// t=0, that order fills when the ask comes to it, it then offers the ask, and
/// that fills when the bid rises through it.
fn book_feed() -> Vec<anyhow::Result<(BookUpdate, NanoTime)>> {
    vec![
        Ok((snapshot(1, "100", "101"), NanoTime::new(0))),
        Ok((
            move_touch(2, ("100", "101"), ("99", "100")),
            NanoTime::new(10),
        )),
        Ok((
            move_touch(3, ("99", "100"), ("99", "101")),
            NanoTime::new(20),
        )),
        Ok((
            move_touch(4, ("99", "101"), ("101", "102")),
            NanoTime::new(30),
        )),
        Ok((
            move_touch(5, ("101", "102"), ("100", "101")),
            NanoTime::new(40),
        )),
    ]
}

// -------------------------------------------------------------------------
// The reference strategy.
// -------------------------------------------------------------------------

/// P0's named reference strategy: quote one unit passively at the touch, flip
/// side once filled, repeat.
///
/// One order in flight at a time, which is what makes it legible: the
/// strategy is "working" from the moment it quotes until the position moves,
/// and the position moving is the only thing that can un-work it. No
/// cancels — P0 has no cancel vocabulary, and adding one to a test instrument
/// would be inventing surface the library does not have.
///
/// It is defined *here* rather than in the library on purpose: a strategy is
/// user code, and a strategy shipped in the engine crate would be the first
/// one that is not.
pub struct ReferenceQuoter;

/// How much the quoter quotes for.
#[derive(Clone, Debug)]
pub struct QuoterCfg {
    qty: Qty,
}

/// Its id counter and in-flight flag.
#[derive(Debug, Default)]
pub struct QuoterState {
    next_id: u64,
    working: bool,
}

#[op(build = reference_quoter)]
impl Op for ReferenceQuoter {
    type Cfg = QuoterCfg;
    type State = QuoterState;
    /// Book value, book tick, position value, position tick. The position
    /// flag is what tells the quoter its order filled; the book flag is what
    /// gives it a price to quote at.
    type In<'a> = (&'a Arc<OrderBook>, bool, &'a Position, bool);
    type Out = Burst<Order>;
    const ACTIVATION: Activation = Activation::NONE;

    fn cycle(
        cfg: &mut QuoterCfg,
        state: &mut QuoterState,
        input: (&Arc<OrderBook>, bool, &Position, bool),
        ctx: &mut Ctx<'_>,
    ) -> anyhow::Result<Tick<Burst<Order>>> {
        let (book, _book_ticked, position, position_ticked) = input;
        if position_ticked {
            state.working = false;
        }
        if state.working {
            return Ok(Tick::Quiet);
        }

        // Flat: bid for one. Long: offer the one we hold back out.
        let (side, level) = if position.is_flat() {
            (Side::Bid, book.best_bid())
        } else {
            (Side::Ask, book.best_ask())
        };
        let Some(level) = level else {
            // No touch (an empty or gapped book) is not a quote.
            return Ok(Tick::Quiet);
        };

        state.next_id += 1;
        state.working = true;
        let order = Order::limit(
            ClOrdId::new(format!("ref-{}", state.next_id)),
            book.instrument().clone(),
            side,
            cfg.qty,
            level.price,
        )
        .with_time_in_force(TimeInForce::GoodTillCancel)
        .with_transact_time(ctx.time());
        Ok(Tick::Value(burst![order]))
    }
}

// -------------------------------------------------------------------------
// The loop.
// -------------------------------------------------------------------------

/// What one run of the loop produced, each entry stamped with the engine time
/// it ticked at.
struct LoopRun {
    orders: Vec<(NanoTime, Burst<Order>)>,
    fills: Vec<(NanoTime, Burst<Fill>)>,
    positions: Vec<(NanoTime, Position)>,
}

/// Wire and run the full loop: replayed book → strategy → orders → simulated
/// venue → fills → position → strategy.
///
/// **The loop is broken on the order edge**, which is the hop a real venue
/// puts distance on: an order emitted at *t* reaches the simulator at *t+1*.
/// That is `feedback`'s structural `time + 1` floor and P0 adds nothing on top
/// of it (§4.2 decision 2).
fn run_loop() -> LoopRun {
    let g = GraphBuilder::new();

    // The venue's side of the order hop: a source with no upstream, which is
    // what keeps the graph acyclic.
    let (orders_at_venue, order_sink) = g.feedback::<Burst<Order>>();

    let book = g.replay_results(book_feed()).order_book();
    let fills = book.sim_venue(&orders_at_venue);
    let position = fills.position();

    let position_handle = position.handle();
    let cfg = QuoterCfg { qty: qty("1") };
    let orders = book.wire(move |b, h| b.reference_quoter(h, position_handle, cfg));

    // Closing the loop. The pass-through is what carries the orders onward.
    let sent = orders.feedback(&order_sink);

    let orders_log = sent.with_time().accumulate();
    let fills_log = fills.with_time().accumulate();
    let positions_log = position.with_time().accumulate();

    let mut r = g.build();
    r.run(HISTORICAL, RunFor::Forever)
        .expect("the loop runs to completion");

    LoopRun {
        orders: r.value(&orders_log),
        fills: r.value(&fills_log),
        positions: r.value(&positions_log),
    }
}

/// **The gate.** The loop closes: the strategy quotes, the quote fills, the
/// position moves, and the strategy acts on the new position — with every
/// value and every tick time exactly what the wiring implies.
#[test]
fn the_order_fill_position_loop_closes() {
    let run = run_loop();

    // Three quotes: the opening bid, the offer once long, and the next bid
    // once flat again.
    let order_times: Vec<u64> = run.orders.iter().map(|(t, _)| u64::from(*t)).collect();
    assert_eq!(order_times, vec![0, 20, 40]);

    let quoted: Vec<_> = run
        .orders
        .iter()
        .map(|(_, b)| {
            assert_eq!(b.len(), 1, "the quoter emits one order per decision");
            (b[0].cl_ord_id.as_str().to_owned(), b[0].side, b[0].price)
        })
        .collect();
    assert_eq!(
        quoted,
        vec![
            ("ref-1".to_owned(), Side::Bid, Some(px("100"))),
            ("ref-2".to_owned(), Side::Ask, Some(px("101"))),
            ("ref-3".to_owned(), Side::Bid, Some(px("100"))),
        ]
    );

    // Two fills. `ref-1` rests at 100 from t=0 and fills once the ask reaches
    // it — but only against the book as of the *previous* instant, so at t=20
    // rather than t=10. `ref-2` offers 101 from t=20 and fills at t=40 for the
    // same reason.
    let filled: Vec<_> = run
        .fills
        .iter()
        .map(|(t, b)| {
            assert_eq!(b.len(), 1);
            (
                u64::from(*t),
                b[0].cl_ord_id.as_str().to_owned(),
                b[0].side,
                b[0].price,
                b[0].qty,
            )
        })
        .collect();
    assert_eq!(
        filled,
        vec![
            (20, "ref-1".to_owned(), Side::Bid, px("100"), qty("1")),
            (40, "ref-2".to_owned(), Side::Ask, px("101"), qty("1")),
        ]
    );

    // Every fill lands strictly after the order that caused it — the
    // `feedback` `time + 1` floor, observed rather than assumed.
    for (fill_time, fills) in &run.fills {
        for fill in fills.iter() {
            let (order_time, _) = run
                .orders
                .iter()
                .find(|(_, b)| b.iter().any(|o| o.cl_ord_id == fill.cl_ord_id))
                .expect("every fill belongs to an order this strategy emitted");
            assert!(
                *fill_time > *order_time,
                "fill for {} at {fill_time:?} did not land after its order at {order_time:?}",
                fill.cl_ord_id
            );
        }
    }

    // The position folds: long one at 100, then flat with a point realized.
    let positions: Vec<_> = run
        .positions
        .iter()
        .map(|(t, p)| (u64::from(*t), p.net_qty(), p.avg_price(), p.realized_pnl()))
        .collect();
    assert_eq!(
        positions,
        vec![
            (20, qty("1"), Some(px("100")), Notional::ZERO),
            (40, Qty::ZERO, None, Notional::parse("1").unwrap()),
        ]
    );

    // And the strategy acted on the fill: `ref-2` is a sell, which it only
    // quotes while long, and it was emitted in the same cycle the fill landed.
    assert_eq!(run.orders[1].0, run.fills[0].0);
}

/// The run is a pure function of its inputs: same wiring, same numbers, every
/// time. This is the property the whole layer is sold on, so it is asserted
/// rather than assumed.
#[test]
fn the_loop_is_deterministic_across_runs() {
    let first = run_loop();
    let second = run_loop();
    assert_eq!(first.orders, second.orders);
    assert_eq!(first.fills, second.fills);
    assert_eq!(first.positions, second.positions);
}

/// §12's open question on the order edge, settled: the feedback edge carries
/// `Burst<Order>`, and the burst arrives intact.
#[test]
fn the_feedback_edge_carries_a_burst_of_orders() {
    let g = GraphBuilder::new();
    let (at_venue, sink) = g.feedback::<Burst<Order>>();
    let book = g.replay_results(book_feed()).order_book();

    // Two orders at one instant, in one burst — which is what the shape is
    // for. Both are marketable against the t=0 book's ask of 101.
    let orders = g.replay_results(vec![
        Ok((marketable_buy("a"), NanoTime::new(10))),
        Ok((marketable_buy("b"), NanoTime::new(10))),
    ]);
    let sent = orders.feedback(&sink);
    let fills = book.sim_venue(&at_venue);

    let sent_log = sent.accumulate();
    let fills_log = fills.with_time().accumulate();
    let mut r = g.build();
    r.run(HISTORICAL, RunFor::Forever).unwrap();

    // One value carrying two orders — not two values.
    let sent: Vec<Burst<Order>> = r.value(&sent_log);
    assert_eq!(sent.len(), 1);
    assert_eq!(sent[0].len(), 2);

    // Both fill, in one burst, one cycle later.
    let fills: Vec<(NanoTime, Burst<Fill>)> = r.value(&fills_log);
    assert_eq!(fills.len(), 1);
    assert_eq!(u64::from(fills[0].0), 11);
    assert_eq!(fills[0].1.len(), 2);
}

/// The `TimeQueue` dedup question (§5.2), pinned. Two orders identical in
/// every field *except* the client order id ride one burst through the
/// feedback edge and both fill — dedup cannot split a burst, and identity
/// closes the remainder.
#[test]
fn two_near_identical_orders_in_one_burst_both_fill() {
    let first = marketable_buy("dup-1");
    let mut second = marketable_buy("dup-2");
    second.transact_time = first.transact_time;
    assert_eq!(
        Order {
            cl_ord_id: first.cl_ord_id.clone(),
            ..second.clone()
        },
        first,
        "the two orders must differ only in their id for this test to mean anything"
    );

    let g = GraphBuilder::new();
    let (at_venue, sink) = g.feedback::<Burst<Order>>();
    let book = g.replay_results(book_feed()).order_book();
    let orders = g.replay_results(vec![
        Ok((first, NanoTime::new(10))),
        Ok((second, NanoTime::new(10))),
    ]);
    let _sent = orders.feedback(&sink);
    let fills_log = book.sim_venue(&at_venue).accumulate();

    let mut r = g.build();
    r.run(HISTORICAL, RunFor::Forever).unwrap();

    let fills: Vec<Burst<Fill>> = r.value(&fills_log);
    assert_eq!(fills.len(), 1);
    let ids: Vec<_> = fills[0]
        .iter()
        .map(|f| f.cl_ord_id.as_str().to_owned())
        .collect();
    assert_eq!(ids, vec!["dup-1".to_owned(), "dup-2".to_owned()]);
}

/// §4.2 decision 1, pinned: an order arriving in the same cycle as the book
/// update that would make it marketable does **not** fill against that update.
/// It sees the book as of the previous instant, and fills on the next book
/// tick.
///
/// Without this, a strategy could react to an update and trade against it at
/// the same instant — lookahead dressed up as speed — and the backtest would
/// flatter every strategy that did.
#[test]
fn an_order_cannot_fill_against_the_update_it_arrived_with() {
    let g = GraphBuilder::new();
    let book = g.replay_results(book_feed()).order_book();

    // A buy at 100 delivered at t=10, the very instant the ask moves to 100.
    // The t=10 book would fill it; the t=0 book (ask 101) does not.
    let order = Order::limit(ClOrdId::new("edge"), inst(), Side::Bid, qty("1"), px("100"))
        .with_time_in_force(TimeInForce::GoodTillCancel)
        .with_transact_time(NanoTime::new(10));
    let orders = g.replay_results(vec![Ok((order, NanoTime::new(10)))]);

    let fills_log = book.sim_venue(&orders).with_time().accumulate();
    let mut r = g.build();
    r.run(HISTORICAL, RunFor::Forever).unwrap();

    let fills: Vec<(NanoTime, Burst<Fill>)> = r.value(&fills_log);
    assert_eq!(fills.len(), 1);
    assert_eq!(
        u64::from(fills[0].0),
        20,
        "the order must fill on the next book tick, not the one it arrived with"
    );
    assert_eq!(fills[0].1[0].price, px("100"));
}

/// A market order sweeps what the previous instant's book offers and emits one
/// fill per level — the burst shape on the fill edge, which §3.3 calls forced.
#[test]
fn a_sweeping_order_fills_in_one_burst_across_levels() {
    let g = GraphBuilder::new();

    // A two-deep book so there is something to sweep.
    let deep = BookUpdate::Snapshot(BookSnapshot {
        instrument: inst(),
        bids: vec![Level::new(px("99"), qty("1"))],
        asks: vec![
            Level::new(px("101"), qty("2")),
            Level::new(px("102"), qty("3")),
        ],
        sequencing: Sequencing::Single(1),
        venue_time: None,
        recv_time: NanoTime::ZERO,
    });
    let book = g
        .replay_results(vec![
            Ok((deep, NanoTime::new(0))),
            Ok((snapshot(2, "99", "101"), NanoTime::new(20))),
        ])
        .order_book();

    let sweep = Order::market(ClOrdId::new("sweep"), inst(), Side::Bid, qty("4"));
    let orders = g.replay_results(vec![Ok((sweep, NanoTime::new(10)))]);
    let fills_log = book.sim_venue(&orders).accumulate();

    let mut r = g.build();
    r.run(HISTORICAL, RunFor::Forever).unwrap();

    let fills: Vec<Burst<Fill>> = r.value(&fills_log);
    assert_eq!(fills.len(), 1, "one tick carrying the whole sweep");
    let legs: Vec<_> = fills[0].iter().map(|f| (f.price, f.qty)).collect();
    assert_eq!(legs, vec![(px("101"), qty("2")), (px("102"), qty("2"))]);
    assert_eq!(fills[0][1].leaves_qty, Qty::ZERO);
}

/// A malformed order aborts the run with context rather than filling
/// nonsensically or being silently dropped.
#[test]
fn an_invalid_order_aborts_the_run() {
    let g = GraphBuilder::new();
    let book = g.replay_results(book_feed()).order_book();

    let mut bad = marketable_buy("bad");
    bad.qty = Qty::ZERO;
    let orders = g.replay_results(vec![Ok((bad, NanoTime::new(10)))]);
    let _fills = book.sim_venue(&orders).accumulate();

    let mut r = g.build();
    let error = r
        .run(HISTORICAL, RunFor::Forever)
        .expect_err("a zero-quantity order is not tradeable");
    assert!(
        format!("{error:#}").contains("must be positive"),
        "{error:#}"
    );
}

/// A buy that crosses the `book_feed` touch of 101 outright.
fn marketable_buy(id: &str) -> Order {
    Order::limit(ClOrdId::new(id), inst(), Side::Bid, qty("1"), px("101"))
        .with_time_in_force(TimeInForce::GoodTillCancel)
        .with_transact_time(NanoTime::new(10))
}
