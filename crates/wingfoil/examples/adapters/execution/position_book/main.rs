//! The position fold on instruments of your own, valued your way, as a
//! node in a graph.
//!
//! `cargo run -p wingfoil --example execution_position_book --features execution`
//!
//! A book holding an equity (linear: PnL in dollars), an index future at
//! fifty dollars a point (linear with a multiplier), an inverse bitcoin
//! future (sized in dollars, settled in bitcoin) and a variance swap it has
//! no model for. `Book::new` is handed how each instrument makes money —
//! its `Measure` — and `total` which settlement currency to add up. The
//! fold holds no formula for any of them, and never withholds a net.
//!
//! The book is node state in the library's position node (`PositionOps`):
//! fills and cashflows are replayed at engine instants into it, it applies
//! each burst in order — fills, then cashflows — and emits what the
//! example reads off the book; the marks arrive on their own stream, and
//! every valuation is printed at the instant it was made.

use std::collections::HashMap;
use std::time::Duration;

use anyhow::Result;
use wingfoil::adapters::execution::edge::Cashflow;
use wingfoil::adapters::execution::exec_id::ExecId;
use wingfoil::adapters::execution::order::{ClientOrderId, Fill, Liquidity};
use wingfoil::adapters::execution::position::{self, Book, Measure, Position, PositionOps};
use wingfoil::adapters::market::{Px, Qty, Side, px, qty};
use wingfoil::prelude::*;
use wingfoil::{NanoTime, RunFor, RunMode};

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash, PartialOrd, Ord)]
enum Instrument {
    /// An equity, in shares, settled in USD.
    #[default]
    Aapl,
    /// An index future, in contracts, at $50 a point.
    Es,
    /// An inverse future, sized in USD, settled in BTC.
    BtcInverse,
    /// Something this book trades but does not value: a variance swap's PnL
    /// runs in realised variance, which no price times a multiplier is.
    VarSwap,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Ccy {
    Usd,
    Btc,
}

/// How each instrument makes money — the caller's knowledge, not the fold's.
fn measure(instrument: &Instrument) -> Option<Measure> {
    match instrument {
        Instrument::Aapl => Some(Measure::Linear),
        // `scaled` refuses a multiplier that is not positive.
        Instrument::Es => Measure::scaled(qty!(50)),
        Instrument::BtcInverse => Some(Measure::Inverse),
        Instrument::VarSwap => None,
    }
}

fn settles_in(instrument: &Instrument) -> Ccy {
    match instrument {
        Instrument::Aapl | Instrument::Es | Instrument::VarSwap => Ccy::Usd,
        Instrument::BtcInverse => Ccy::Btc,
    }
}

/// What the example reads off the book after each instant: every
/// position, and how many fills moved one nothing values. `Book` itself has
/// no `Default`, so it never rides an edge; a reading of it does.
#[derive(Clone, Debug, Default)]
struct Held {
    positions: Vec<Position<Instrument>>,
    unvalued: u64,
}

fn held(book: &Book<Instrument>) -> Held {
    Held {
        positions: book.positions(),
        unvalued: book.unvalued(),
    }
}

const START: NanoTime = NanoTime::ZERO;

fn at(secs: u64) -> NanoTime {
    START + Duration::from_secs(secs)
}

fn secs(t: NanoTime) -> u64 {
    u64::from(t - START) / 1_000_000_000
}

fn fill(
    instrument: Instrument,
    side: Side,
    qty: Qty,
    price: Px,
    fee: Qty,
    at: NanoTime,
) -> (Fill<Instrument>, NanoTime) {
    let fill = Fill {
        order: ClientOrderId(1),
        exec_id: ExecId::new("x").unwrap(),
        instrument,
        side,
        qty,
        filled: qty,
        remaining: Qty::ZERO,
        price,
        fee,
        liquidity: Liquidity::Taker,
        venue_time: None,
        recv_time: at,
    };
    (fill, at)
}

/// Everything this graph is fed from outside it, replayed from rows here.
/// Live, each field comes from its real source instead — the fills and
/// cashflows off the venue's session, the marks off its mark feed — and the
/// wiring in `main` does not change.
struct Inputs {
    /// The book's fills.
    fills: Stream<Burst<Fill<Instrument>>>,
    /// Money moved with nothing traded.
    cashflows: Stream<Burst<Cashflow<Instrument>>>,
    /// The venue's marks, a burst per instant.
    marks: Stream<Burst<(Instrument, Px)>>,
}

impl Inputs {
    fn replayed(g: &GraphBuilder) -> Self {
        let fills = g.replay(vec![
            // 100 shares at 190, 50 sold back at 195: $250 realised.
            fill(
                Instrument::Aapl,
                Side::Bid,
                qty!(100),
                px!(190),
                qty!(1),
                at(1),
            ),
            fill(
                Instrument::Aapl,
                Side::Ask,
                qty!(50),
                px!(195),
                qty!(0.5),
                at(2),
            ),
            // $60,000 of inverse future bought at 60,000, and two index
            // futures at 5000.00: $500,000 of notional. A variance swap too.
            fill(
                Instrument::BtcInverse,
                Side::Bid,
                qty!(60_000),
                px!(60_000),
                qty!(0.00001),
                at(3),
            ),
            fill(
                Instrument::Es,
                Side::Bid,
                qty!(2),
                px!(5000.00),
                qty!(4),
                at(3),
            ),
            fill(
                Instrument::VarSwap,
                Side::Bid,
                qty!(10),
                px!(20),
                qty!(0),
                at(3),
            ),
        ]);
        // Funding paid on the inverse future: money moved, nothing traded.
        let cashflows = g.replay(vec![(
            Cashflow {
                instrument: Instrument::BtcInverse,
                amount: qty!(-0.00002),
                venue_time: None,
                recv_time: at(4),
            },
            at(4),
        )]);
        // The marks, as the venue publishes them: a burst per instant.
        let marks = g.replay(vec![
            ((Instrument::Aapl, px!(200)), at(5)),
            ((Instrument::Es, px!(5001.25)), at(5)),
            ((Instrument::VarSwap, px!(21)), at(5)),
            ((Instrument::BtcInverse, px!(66_000)), at(5)),
            ((Instrument::BtcInverse, px!(54_000)), at(6)),
            ((Instrument::BtcInverse, px!(63_000)), at(7)),
        ]);
        Inputs {
            fills,
            cashflows,
            marks,
        }
    }
}

fn main() -> Result<()> {
    let g = GraphBuilder::new();
    let Inputs {
        fills,
        cashflows,
        marks,
    } = Inputs::replayed(&g);

    // Folded into the latest mark per instrument.
    let marks = marks.fold(HashMap::new(), |marks, burst: &Burst<(Instrument, Px)>| {
        for (instrument, mark) in burst {
            marks.insert(*instrument, *mark);
        }
    });

    let book = fills
        .wire_position(measure)
        .cashflows(&cashflows)
        .reading(held)
        .build();

    // Each fold, as it happens: what is held and what it is carried at.
    let _held = book.with_time().for_each(|(t, book)| {
        println!("t={} book:", secs(*t));
        // In the instrument's order, so a log line and its replay agree.
        for position in &book.positions {
            println!(
                "  {:?}: net {}, entry {}, realised {}, fees {}, cashflows {}",
                position.instrument,
                position.net(),
                position.entry().map_or("-".to_string(), |p| p.to_string()),
                position.realised(),
                position.fees(),
                position.cashflows()
            );
        }
        println!("  unvalued fills: {}", book.unvalued);
        Ok(())
    });

    // Each mark, read against the book it finds: the inverse future is not
    // linear (up 10% makes less than down 10% loses), the index future's
    // PnL is in dollars not points, the variance swap is held but not
    // valued, and a total is one currency at a time — refused rather than
    // quietly short where an open position has no value.
    let _valued = marks
        .join_passive(&book, |marks, book| (marks.clone(), book.clone()))
        .with_time()
        .for_each(|(t, (marks, book))| {
            println!("t={} marked:", secs(*t));
            for position in &book.positions {
                let Some(mark) = marks.get(&position.instrument) else {
                    continue;
                };
                println!(
                    "  {:?} @ {mark}: unrealised {}",
                    position.instrument,
                    position
                        .unrealised(*mark)
                        .map_or("none (no measure)".to_string(), |u| u.to_string())
                );
            }
            let mark = |i: &Instrument| marks.get(i).copied();
            for ccy in [Ccy::Usd, Ccy::Btc] {
                match position::total(&book.positions, |i| settles_in(i) == ccy, mark) {
                    Some(total) => println!(
                        "  {ccy:?} total: net {} (realised {}, unrealised {}, fees {}, cashflows {})",
                        total.net(),
                        total.realised,
                        total.unrealised,
                        total.fees,
                        total.cashflows
                    ),
                    None => println!("  {ccy:?} total: none — an open position cannot be valued"),
                }
            }
            Ok(())
        });

    let mut runner = g.build();
    runner.run(RunMode::HistoricalFrom(START), RunFor::Forever)?;
    Ok(())
}
