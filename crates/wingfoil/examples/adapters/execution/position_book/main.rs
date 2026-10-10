//! The position fold on instruments of your own, valued your way.
//!
//! `cargo run -p wingfoil --example execution_position_book --features execution`
//!
//! A book holding an equity (linear: PnL in dollars), an index future at
//! fifty dollars a point (linear with a multiplier), an inverse bitcoin
//! future (sized in dollars, settled in bitcoin) and a variance swap it has no
//! model for. `Book::new` is handed how each instrument makes money — its
//! `Measure` — and `total` which settlement currency to add up. The fold
//! holds no formula for any of them, and never withholds a net.

use wingfoil::NanoTime;
use wingfoil::adapters::execution::exec_id::ExecId;
use wingfoil::adapters::execution::order::{ClientOrderId, Fill, Liquidity};
use wingfoil::adapters::execution::position::{Book, Measure};
use wingfoil::adapters::market::{Px, Qty, Side};

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
        Instrument::Es => Measure::scaled(q("50")),
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

fn q(s: &str) -> Qty {
    Qty::parse(s).unwrap()
}

fn p(s: &str) -> Px {
    Px::parse(s).unwrap()
}

fn fill(instrument: Instrument, side: Side, qty: &str, price: &str, fee: &str) -> Fill<Instrument> {
    Fill {
        order: ClientOrderId(1),
        exec_id: ExecId::new("x").unwrap(),
        instrument,
        side,
        qty: q(qty),
        filled: q(qty),
        remaining: Qty::ZERO,
        price: p(price),
        fee: q(fee),
        liquidity: Liquidity::Taker,
        venue_time: None,
        recv_time: NanoTime::ZERO,
    }
}

fn main() {
    let mut book = Book::new(measure);
    book.apply(&[
        // 100 shares at 190, 50 sold back at 195: $250 realised.
        fill(Instrument::Aapl, Side::Bid, "100", "190", "1"),
        fill(Instrument::Aapl, Side::Ask, "50", "195", "0.5"),
        // $60,000 of inverse future bought at 60,000.
        fill(
            Instrument::BtcInverse,
            Side::Bid,
            "60000",
            "60000",
            "0.00001",
        ),
        // Two index futures at 5000.00: $500,000 of notional.
        fill(Instrument::Es, Side::Bid, "2", "5000.00", "4"),
        fill(Instrument::VarSwap, Side::Bid, "10", "20", "0"),
    ]);
    book.cashflow(Instrument::BtcInverse, q("-0.00002"));

    let aapl = book.position(&Instrument::Aapl).unwrap();
    println!(
        "AAPL: net {}, entry {:?}, realised ${}",
        aapl.net(),
        aapl.entry(),
        aapl.realised()
    );
    assert_eq!(aapl.realised(), q("250"));

    // The inverse future is not linear: up 10% makes less than down 10%
    // loses, which a straight line in the price would hide.
    let btc = book.position(&Instrument::BtcInverse).unwrap();
    let up = btc.unrealised(p("66000")).unwrap();
    let down = btc.unrealised(p("54000")).unwrap();
    println!("BTC inverse: +10% → {up} BTC, −10% → {down} BTC");
    assert!(down.raw().abs() > up.raw().abs());

    // The index future's PnL is in dollars, not points: a quarter-point
    // move on two contracts is 1.25 × 2 × 50, and the entry reads back in
    // points.
    let es = book.position(&Instrument::Es).unwrap();
    let es_up = es.unrealised(p("5001.25")).unwrap();
    println!(
        "ES: net {}, entry {:?}, +1.25 points → ${es_up}",
        es.net(),
        es.entry()
    );
    assert_eq!(es_up, q("125"));

    // A variance swap with no measure is held, not valued, and counted as
    // such.
    println!(
        "variance swap: net {}, pnl {:?}, unvalued fills {}",
        book.net(&Instrument::VarSwap),
        book.position(&Instrument::VarSwap).unwrap().pnl(p("21")),
        book.unvalued()
    );

    // Totals are one currency at a time; one with an unmarked open
    // position is refused rather than quietly short.
    let mark = |i: &Instrument| match i {
        Instrument::Aapl => Some(p("200")),
        Instrument::Es => Some(p("5001.25")),
        Instrument::BtcInverse => Some(p("63000")),
        Instrument::VarSwap => Some(p("21")),
    };
    let btc_total = book.total(|i| settles_in(i) == Ccy::Btc, mark).unwrap();
    println!(
        "BTC total: net {} (cashflows {}, fees {})",
        btc_total.net(),
        btc_total.cashflows,
        btc_total.fees
    );
    let usd_total = book.total(|i| settles_in(i) == Ccy::Usd, mark);
    println!("USD total: {usd_total:?} — the variance swap cannot be valued, so no total");
    assert_eq!(usd_total, None);

    // Sorted, so a log line and its replay agree.
    for position in book.positions() {
        println!("  {:?}: {}", position.instrument, position.net());
    }
}
