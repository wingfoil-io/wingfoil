#![cfg(feature = "execution-exchange")]
//! The simulated exchange wired as a graph: participant requests in, reports,
//! market data and a ledger out, under `RunMode::HistoricalFrom` so values
//! **and** tick times are pinned.
//!
//! The matching rules themselves are unit-tested beside the op
//! (`src/adapters/execution/exchange.rs`); this file pins the wiring — that
//! the op, its output splitters and the ledger compose on the ordinary engine,
//! and that a run is deterministic.

use std::sync::Arc;

use wingfoil::adapters::execution::exchange::{
    ExchangeConfig, ExchangeOps, ExchangeOutputOps, FeeRate, FeeSchedule, InstrumentSpec, Ledger,
    LedgerOps, Report, Request,
};
use wingfoil::adapters::execution::{AccountId, ClOrdId, Notional, Order};
use wingfoil::adapters::market::{InstrumentId, MarketEvent, Px, Qty, Side};
use wingfoil::prelude::*;
use wingfoil::{NanoTime, RunFor, RunMode};

fn inst() -> InstrumentId {
    InstrumentId::new("arena", "BTC-PERP")
}

fn px(s: &str) -> Px {
    Px::parse(s).unwrap()
}

fn qty(s: &str) -> Qty {
    Qty::parse(s).unwrap()
}

fn limit(account: &str, id: &str, side: Side, q: &str, p: &str) -> Request {
    Request::New {
        account: AccountId::new(account),
        order: Order::limit(ClOrdId::new(id), inst(), side, qty(q), px(p)),
    }
}

fn market(account: &str, id: &str, side: Side, q: &str) -> Request {
    Request::New {
        account: AccountId::new(account),
        order: Order::market(ClOrdId::new(id), inst(), side, qty(q)),
    }
}

fn config() -> ExchangeConfig {
    ExchangeConfig::new(
        "arena",
        vec![
            InstrumentSpec::new(inst(), px("0.5"), qty("0.001")).with_fees(FeeSchedule {
                maker: FeeRate::ZERO,
                taker: FeeRate::bps(10),
            }),
        ],
    )
    .with_book_depth(3)
}

/// Two makers quote, a taker lifts both, one maker then cancels the rest.
fn script() -> Vec<anyhow::Result<(Request, NanoTime)>> {
    let t = NanoTime::new;
    vec![
        Ok((limit("mm1", "a", Side::Ask, "1", "100"), t(10))),
        Ok((limit("mm2", "b", Side::Ask, "2", "100.5"), t(10))),
        Ok((limit("mm1", "c", Side::Bid, "1", "99"), t(20))),
        Ok((market("taker", "x", Side::Bid, "1.5"), t(30))),
        Ok((
            Request::Cancel {
                account: AccountId::new("mm2"),
                cl_ord_id: ClOrdId::new("b"),
            },
            t(40),
        )),
    ]
}

struct Run {
    reports: Vec<(NanoTime, Vec<Report>)>,
    trades: Vec<(NanoTime, Px, Qty)>,
    ledger: Arc<Ledger>,
}

fn run() -> Run {
    let g = GraphBuilder::new();
    let out = g.replay_results(script()).exchange(config());
    let reports = out.reports();
    let ledger = reports.ledger();
    let collected = reports
        .map(|b: &Burst<Report>| b.to_vec())
        .with_time()
        .accumulate();
    let trades = out
        .market_events()
        .map(|b: &Burst<MarketEvent>| {
            b.iter()
                .filter_map(|e| match e {
                    MarketEvent::Trade(t) => Some((t.price, t.qty)),
                    _ => None,
                })
                .collect::<Vec<_>>()
        })
        .with_time()
        .accumulate();
    let mut runner = g.build();
    runner
        .run(RunMode::HistoricalFrom(NanoTime::ZERO), RunFor::Forever)
        .unwrap();
    Run {
        reports: runner.value(&collected),
        trades: runner
            .value(&trades)
            .into_iter()
            .flat_map(|(t, v)| v.into_iter().map(move |(p, q)| (t, p, q)))
            .collect(),
        ledger: runner.value(&ledger),
    }
}

#[test]
fn reports_tick_at_the_request_times() {
    let r = run();
    let times: Vec<u64> = r.reports.iter().map(|(t, _)| u64::from(*t)).collect();
    assert_eq!(times, vec![10, 20, 30, 40]);
    // t=30: accepted, then two maker/taker fill pairs.
    let at_30 = &r.reports[2].1;
    assert!(matches!(at_30[0], Report::Accepted { .. }));
    assert_eq!(
        at_30
            .iter()
            .filter(|r| matches!(r, Report::Filled { .. }))
            .count(),
        4
    );
    // t=40: mm2's remaining 1.5 cancelled.
    assert!(matches!(
        &r.reports[3].1[0],
        Report::Cancelled { leaves_qty, .. } if *leaves_qty == qty("1.5")
    ));
}

#[test]
fn trades_print_at_resting_prices_in_priority_order() {
    let r = run();
    let t30 = NanoTime::new(30);
    assert_eq!(
        r.trades,
        vec![(t30, px("100"), qty("1")), (t30, px("100.5"), qty("0.5"))]
    );
}

#[test]
fn the_ledger_nets_to_zero_across_accounts() {
    let r = run();
    let pos = |a: &str| {
        r.ledger
            .position(&AccountId::new(a), &inst())
            .map(|p| p.net_qty())
    };
    assert_eq!(pos("taker"), Some(qty("1.5")));
    assert_eq!(pos("mm1"), Some(qty("-1")));
    assert_eq!(pos("mm2"), Some(qty("-0.5")));
    let taker = r
        .ledger
        .position(&AccountId::new("taker"), &inst())
        .unwrap();
    // 10bp on 100 + 50.25 notional.
    assert_eq!(taker.fees(), Notional::parse("0.15025").unwrap());
}

#[test]
fn a_run_is_deterministic() {
    let (a, b) = (run(), run());
    assert_eq!(a.reports, b.reports);
    assert_eq!(a.trades, b.trades);
    assert_eq!(a.ledger, b.ledger);
}
