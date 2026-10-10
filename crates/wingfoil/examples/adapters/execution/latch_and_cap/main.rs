//! The kill switch over limits of your own, and a per-order ceiling.
//!
//! `cargo run -p wingfoil --example execution_latch_and_cap --features execution`
//!
//! An equity book with three limits — gross exposure, the day's loss, and
//! a feed that went quiet. `Switch` owns the latch: a breach halts until an
//! operator clears it, `clear` refuses while one stands, and a restart
//! carries it. The caps and what they compare are the caller's. The
//! ceiling judges each order against a per-contract cap before a venue sees
//! it.

use std::time::Duration;

use wingfoil::NanoTime;
use wingfoil::adapters::execution::ceiling::{Breach, Cap, Judge};
use wingfoil::adapters::execution::edge::Request;
use wingfoil::adapters::execution::kill_switch::{Breaches, Limit, Stance, Switch, within};
use wingfoil::adapters::execution::order::{ClientOrderId, Order};
use wingfoil::adapters::market::{Px, Qty, Side};

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
enum Risk {
    Gross,
    DailyLoss,
    Stale,
}

impl Limit for Risk {
    const ALL: &'static [Risk] = &[Risk::Gross, Risk::DailyLoss, Risk::Stale];
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash, PartialOrd, Ord)]
enum Ticker {
    #[default]
    Aapl,
    Penny,
}

/// At most 500 shares an order, and nothing at all in penny stocks.
#[derive(Clone, Copy, Debug)]
struct Clip;

impl Cap<Ticker> for Clip {
    fn of(&self, instrument: &Ticker) -> Option<Qty> {
        (*instrument == Ticker::Aapl).then(|| Qty::parse("500").unwrap())
    }
}

/// US equities: the day starts at the 09:30 ET open, 13:30 UTC.
const OPEN: Duration = Duration::from_secs(13 * 3600 + 30 * 60);

fn at(secs: u64) -> NanoTime {
    NanoTime::from(20_000 * 86_400 * 1_000_000_000 + (13 * 3600 + 30 * 60 + secs) * 1_000_000_000)
}

/// One assessment: the caller's caps over what it computed.
fn assess(
    switch: &mut Switch<Risk>,
    now: NanoTime,
    gross: f64,
    equity: Option<Qty>,
    fresh: bool,
) -> Breaches<Risk> {
    let mut standing = Breaches::NONE;
    if !within(gross, 1_000_000.0) {
        standing.set(Risk::Gross);
    }
    // An equity nobody knows is a breach, not a pass.
    if !switch
        .daily_loss(now, equity)
        .is_some_and(|(fraction, _)| fraction <= 0.02)
    {
        standing.set(Risk::DailyLoss);
    }
    if !fresh {
        standing.set(Risk::Stale);
    }
    switch.record(now, standing);
    standing
}

fn eq(s: &str) -> Option<Qty> {
    Some(Qty::parse(s).unwrap())
}

fn main() {
    let mut switch = Switch::<Risk>::new(OPEN);
    assess(&mut switch, at(0), 400_000.0, eq("1000000"), true);
    println!(
        "open:  {:?}, day opened at {:?}",
        switch.stance(),
        switch.opening_equity()
    );
    assert_eq!(switch.stance(), Stance::Open);

    // A gap: gross exposure through its cap and 3% down on the day.
    let standing = assess(&mut switch, at(60), 1_200_000.0, eq("970000"), true);
    println!("gap:   {:?}, standing {standing:?}", switch.stance());

    // The market comes back: nothing stands, but the latch outlives it.
    assess(&mut switch, at(120), 500_000.0, eq("995000"), true);
    println!(
        "after: {:?}, latched {:?} since {:?}",
        switch.stance(),
        switch.latched(),
        switch.latch().map(|l| l.at)
    );
    assert_eq!(switch.stance(), Stance::Halted);

    // A NaN exposure is outside every cap, and a stale feed is a breach.
    let standing = assess(&mut switch, at(180), f64::NAN, eq("995000"), false);
    println!("nan:   standing {standing:?}; clear → {:?}", switch.clear());

    // A restart carries the latch and the day; it does not release them.
    let restarted = Switch::restore(OPEN, switch.snapshot());
    assert_eq!(restarted.latched(), switch.latched());
    println!("restart: still {:?}", restarted.stance());

    // Inside again, an operator clears it.
    assess(&mut switch, at(240), 500_000.0, eq("995000"), true);
    switch.clear().unwrap();
    println!("clear: {:?}", switch.stance());

    // The ceiling: per order, before the venue.
    let judge = Judge::new(Clip);
    let order = |ticker, size: &str| {
        Request::Place(Order::limit(
            ClientOrderId(1),
            ticker,
            Side::Bid,
            Qty::parse(size).unwrap(),
            Px::parse("190").unwrap(),
        ))
    };
    println!("500 AAPL → {:?}", judge.judge(&order(Ticker::Aapl, "500")));
    println!("501 AAPL → {:?}", judge.judge(&order(Ticker::Aapl, "501")));
    assert!(matches!(
        judge.judge(&order(Ticker::Penny, "1")),
        Err(Breach::Unbounded { .. })
    ));
    println!("1 PENNY → refused, nothing bounds it");
}
