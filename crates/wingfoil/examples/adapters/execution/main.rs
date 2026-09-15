//! The execution loop end to end: a passive quoting strategy trading against a
//! simulated venue, over a replayed order book.
//!
//! ```sh
//! cargo run --example execution_adapter --features execution-sim
//! ```
//!
//! The point of the example is the *shape* of the graph, not the strategy. The
//! strategy consumes books and positions and emits orders; what sits on the
//! other side of the order stream — [`sim_venue`](wingfoil::adapters::execution::sim::SimVenueOps::sim_venue)
//! here, a venue's own protocol live — is a wiring decision the strategy never
//! sees.
//!
//! It runs under `RunMode::HistoricalFrom`, so the output below is the same on
//! every run.
//!
//! **The fill model is deliberately dishonest** — fill at touch, no queue
//! position, no fees, no market impact. See the module docs on
//! `adapters::execution::sim`: gate P0 of Project Venue exists to prove the
//! loop closes, and an honest model is gate P2. Do not read the PnL below as a
//! result.

use std::sync::Arc;

use anyhow::Result;
use wingfoil::adapters::execution::sim::SimVenueOps;
use wingfoil::adapters::execution::{ClOrdId, Fill, Order, Position, PositionOps, TimeInForce};
use wingfoil::adapters::market::{
    BookDelta, BookSnapshot, BookUpdate, InstrumentId, Level, LevelChange, MarketBookOps,
    OrderBook, Px, Qty, Sequencing, Side,
};
use wingfoil::op;
use wingfoil::op::{Activation, Ctx, Op, Tick};
use wingfoil::prelude::*;
use wingfoil::{Burst, NanoTime, RunFor, RunMode, burst};

/// Quote one unit passively at the touch; once filled, offer it back out.
///
/// One order in flight at a time — the strategy is "working" from the moment
/// it quotes until the position moves, and the position moving is the only
/// thing that un-works it.
struct Quoter;

/// How much to quote for.
#[derive(Clone, Debug)]
struct QuoterCfg {
    qty: Qty,
}

/// The id counter and the in-flight flag.
#[derive(Debug, Default)]
struct QuoterState {
    next_id: u64,
    working: bool,
}

#[op(build = quoter)]
impl Op for Quoter {
    type Cfg = QuoterCfg;
    type State = QuoterState;
    /// Book value, book tick, position value, position tick. The position flag
    /// is how the strategy learns its order filled.
    type In<'a> = (&'a Arc<OrderBook>, bool, &'a Position, bool);
    type Out = Burst<Order>;
    const ACTIVATION: Activation = Activation::NONE;

    fn cycle(
        cfg: &mut QuoterCfg,
        state: &mut QuoterState,
        input: (&Arc<OrderBook>, bool, &Position, bool),
        ctx: &mut Ctx<'_>,
    ) -> Result<Tick<Burst<Order>>> {
        let (book, _book_ticked, position, position_ticked) = input;
        if position_ticked {
            state.working = false;
        }
        if state.working {
            return Ok(Tick::Quiet);
        }

        let (side, level) = if position.is_flat() {
            (Side::Bid, book.best_bid())
        } else {
            (Side::Ask, book.best_ask())
        };
        let Some(level) = level else {
            return Ok(Tick::Quiet);
        };

        state.next_id += 1;
        state.working = true;
        let order = Order::limit(
            ClOrdId::new(format!("q-{}", state.next_id)),
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

fn main() -> Result<()> {
    let instrument = InstrumentId::new("simvenue", "BTC-USD");
    let g = GraphBuilder::new();

    // The venue's side of the order hop. `feedback` is what makes the cycle a
    // DAG, and its `time + 1` is why a strategy can never see a fill at the
    // instant it ordered.
    let (orders_at_venue, order_sink) = g.feedback::<Burst<Order>>();

    let book = g.replay_results(feed(&instrument)).order_book();
    let fills = book.sim_venue(&orders_at_venue);
    let position = fills.position();

    let position_handle = position.handle();
    let cfg = QuoterCfg {
        qty: Qty::parse("1")?,
    };
    let orders = book.wire(move |b, h| b.quoter(h, position_handle, cfg));
    let sent = orders.feedback(&order_sink);

    // Print as the run progresses rather than accumulating: the same wiring
    // then works against a live feed unchanged.
    let _quotes = sent
        .with_time()
        .for_each(|(t, orders): &(NanoTime, Burst<Order>)| {
            for order in orders.iter() {
                println!(
                    "{:>3}ns  quote  {} {} {} @ {}",
                    u64::from(*t),
                    order.cl_ord_id,
                    side_word(order.side),
                    order.qty,
                    order.price.expect("the quoter only sends limit orders"),
                );
            }
            Ok(())
        });

    let _executions = fills
        .with_time()
        .for_each(|(t, fills): &(NanoTime, Burst<Fill>)| {
            for fill in fills.iter() {
                println!(
                    "{:>3}ns  FILL   {} {} {} @ {}",
                    u64::from(*t),
                    fill.cl_ord_id,
                    side_word(fill.side),
                    fill.qty,
                    fill.price,
                );
            }
            Ok(())
        });

    let _pnl = position
        .with_time()
        .for_each(|(t, position): &(NanoTime, Position)| {
            println!(
                "{:>3}ns  pos    net {} @ {}  realized {}",
                u64::from(*t),
                position.net_qty(),
                position
                    .avg_price()
                    .map_or_else(|| "-".to_owned(), |p| p.to_string()),
                position.realized_pnl(),
            );
            Ok(())
        });

    let mut runner = g.build();
    runner.run(RunMode::HistoricalFrom(NanoTime::ZERO), RunFor::Forever)?;
    Ok(())
}

fn side_word(side: Side) -> &'static str {
    match side {
        Side::Bid => "buy ",
        Side::Ask => "sell",
    }
}

/// A one-deep book that walks down and back up, so a passive quote on each
/// side gets reached in turn.
fn feed(instrument: &InstrumentId) -> Vec<Result<(BookUpdate, NanoTime)>> {
    let snapshot = |bid: &str, ask: &str| -> Result<BookUpdate> {
        Ok(BookUpdate::Snapshot(BookSnapshot {
            instrument: instrument.clone(),
            bids: vec![Level::new(Px::parse(bid)?, Qty::parse("1")?)],
            asks: vec![Level::new(Px::parse(ask)?, Qty::parse("1")?)],
            sequencing: Sequencing::Single(1),
            venue_time: None,
            recv_time: NanoTime::ZERO,
        }))
    };
    let move_touch = |seq: u64, from: (&str, &str), to: (&str, &str)| -> Result<BookUpdate> {
        Ok(BookUpdate::Delta(BookDelta {
            instrument: instrument.clone(),
            changes: vec![
                LevelChange::new(Side::Bid, Px::parse(from.0)?, Qty::ZERO),
                LevelChange::new(Side::Ask, Px::parse(from.1)?, Qty::ZERO),
                LevelChange::new(Side::Bid, Px::parse(to.0)?, Qty::parse("1")?),
                LevelChange::new(Side::Ask, Px::parse(to.1)?, Qty::parse("1")?),
            ],
            sequencing: Sequencing::Single(seq),
            venue_time: None,
            recv_time: NanoTime::ZERO,
        }))
    };

    let steps: Vec<Result<BookUpdate>> = vec![
        snapshot("100", "101"),
        move_touch(2, ("100", "101"), ("99", "100")),
        move_touch(3, ("99", "100"), ("99", "101")),
        move_touch(4, ("99", "101"), ("101", "102")),
        move_touch(5, ("101", "102"), ("100", "101")),
    ];
    steps
        .into_iter()
        .enumerate()
        .map(|(i, step)| step.map(|update| (update, NanoTime::new(i as u64 * 10))))
        .collect()
}
