//! The execution layer, interpreted vs `nitro!`: what one cycle of a quoting
//! loop costs on each tier.
//!
//! One wiring, a quoter at steady state, every node firing every cycle:
//!
//! - a touch that moves each cycle, quoted as a one-lot bid;
//! - the OMS (`oms_op`), which places that bid;
//! - the FIX bridge (`fix_bridge_op`), rendering what the OMS sends;
//! - the position fold (`position`) over the fills;
//! - the kill switch (`kill_switch`) on the net the fold reads.
//!
//! `nitro!` cannot close a loop (that needs `feedback`, which a compiled
//! graph cannot express), so the venue is played open-loop: cycle `n` acks,
//! then fills whole at its price, the order cycle `n - 1` placed — on the
//! OMS's edge as `Report`s and on the bridge's as FIX `ExecReport`s. Ids are
//! a sequence under one epoch on both, so that order is
//! `ClientOrderId::new(ZERO, n - 1)` and `ClOrdId(n - 1)`. The fill frees the
//! bid slot before the diff, so the OMS places a fresh order every cycle, and
//! ends the order at the bridge, so its maps stay at one live order rather
//! than growing for the run. The setup asserts both, so a change that leaves
//! the OMS idle fails here rather than reading as a speed-up.
//!
//! Three tiers on the identical wiring: `interpreted()`, `compiled()` (the
//! whole graph as one function) and `nested()` (the same compiled body as an
//! island inside an interpreted graph).

use std::sync::LazyLock;
use std::time::Duration;

use criterion::{Criterion, Throughput, criterion_group, criterion_main};
use std::hint::black_box;
use wingfoil::prelude::*;
use wingfoil::trading::execution::edge::{Ack, Cashflow, Report, Request, TradingState};
use wingfoil::trading::execution::exec_id::ExecId;
use wingfoil::trading::execution::fix::node::*;
use wingfoil::trading::execution::fix::{ClOrdId, ExecKind, ExecReport, Message, Trade};
use wingfoil::trading::execution::kill_switch::node::*;
use wingfoil::trading::execution::kill_switch::{Assessed, Breaches, Limit, Switch};
use wingfoil::trading::execution::oms::node::*;
use wingfoil::trading::execution::oms::{Config, Desired, Ladder, Oms, Pacing};
use wingfoil::trading::execution::order::{ClientOrderId, Epoch, Fill, Liquidity};
use wingfoil::trading::execution::position::node::*;
use wingfoil::trading::execution::position::{Book, Measure};
use wingfoil::trading::market::{Level, Px, Qty, Side};
use wingfoil::{NanoTime, RunFor, RunMode};

const HISTORICAL: RunMode = RunMode::HistoricalFrom(NanoTime::ZERO);
const PERIOD: Duration = Duration::from_millis(1);
const CYCLES: u32 = 10_000;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct Es;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Risk {
    MaxLong,
}

impl Limit for Risk {
    const ALL: &'static [Risk] = &[Risk::MaxLong];
}

type OmsOut = (Burst<Request<Es>>, Pacing, ());
/// `(requests sent, messages rendered, reports the bridge translated, net,
/// the switch)`, on the last cycle.
type Cycle = (usize, usize, usize, Qty, Assessed<Risk>);

fn one<T: Default>(value: T) -> Burst<T> {
    let mut burst = Burst::new();
    burst.push(value);
    burst
}

/// The bid at cycle `n`: 5000 walking up a quarter a cycle, four ticks
/// round, so every cycle quotes a price the last did not.
///
/// Prices and sizes are `const`, so the cycle pays for the layer and not
/// for parsing its inputs.
fn bid(n: u64) -> Px {
    const BIDS: [Px; 4] = [
        Px::parse_const("5000"),
        Px::parse_const("5000.25"),
        Px::parse_const("5000.5"),
        Px::parse_const("5000.75"),
    ];
    BIDS[(n % 4) as usize]
}

const LOT: Qty = Qty::parse_const("1");

fn config() -> Config {
    Config::unmetered().with_max_desired_age(Duration::from_secs(60))
}

/// The bid the quoter wants at cycle `n`.
fn quote(n: u64, now: NanoTime) -> Burst<Desired<Es>> {
    one(Desired::rest(
        Es,
        now,
        Side::Bid,
        Ladder::one(Level::new(bid(n), LOT)),
    ))
}

/// The order placed at cycle `n - 1`, which the venue answers at `n`, if
/// there is one. Both the OMS's ids and the bridge's are a bare sequence
/// under `Epoch::ZERO`, one per order, so cycle `k`'s order is `k` on each.
fn answered(n: u64) -> Option<u64> {
    n.checked_sub(1).filter(|&k| k >= 1)
}

/// The venue, open-loop, as the OMS hears it: at cycle `n` the order placed
/// at `n - 1` is acked, then fills whole at its price — the ordinary
/// place → ack → fill path, in one burst.
fn venue_reports(n: u64, now: NanoTime) -> Burst<Report<Es>> {
    let mut reports = Burst::new();
    if let Some(k) = answered(n) {
        let order = ClientOrderId::new(Epoch::ZERO, k as u32);
        reports.push(Report::Ack(Ack {
            order,
            price: Some(bid(k)),
            remaining: Some(LOT),
            recv_time: now,
            ..Default::default()
        }));
        reports.push(Report::Fill(Fill {
            order,
            exec_id: *EXEC_ID,
            instrument: Es,
            side: Side::Bid,
            qty: LOT,
            filled: LOT,
            remaining: Qty::ZERO,
            price: bid(k),
            liquidity: Liquidity::Maker,
            recv_time: now,
            ..Default::default()
        }));
    }
    reports
}

/// The same answer as the FIX bridge hears it: `New`, then the trade that
/// ends the order, so the bridge forgets it and its maps stay bounded.
fn venue_executions(n: u64, now: NanoTime) -> Burst<ExecReport<Es>> {
    let mut executions = Burst::new();
    if let Some(k) = answered(n) {
        let cl_ord_id = ClOrdId(k);
        executions.push(ExecReport {
            cl_ord_id,
            orig: None,
            kind: ExecKind::New,
            venue_time: now,
        });
        executions.push(ExecReport {
            cl_ord_id,
            orig: None,
            kind: ExecKind::Trade(Trade {
                exec_id: *EXEC_ID,
                instrument: Es,
                side: Side::Bid,
                qty: LOT,
                filled: LOT,
                remaining: Qty::ZERO,
                price: bid(k),
                liquidity: Liquidity::Maker,
            }),
            venue_time: now,
        });
    }
    executions
}

fn fills_of(reports: &Burst<Report<Es>>) -> Burst<Fill<Es>> {
    reports
        .iter()
        .filter_map(|r| match r {
            Report::Fill(fill) => Some(*fill),
            _ => None,
        })
        .collect()
}

/// Built once: an `ExecId` is parsed from text, and the cycle should pay
/// for the layer, not for its inputs.
static EXEC_ID: LazyLock<ExecId> = LazyLock::new(|| ExecId::new("x").expect("literal exec id"));

/// The OMS is read for nothing: the requests are the output.
fn read_nothing(_: &Oms<Es>) {}

fn es_measure(_: &Es) -> Option<Measure> {
    const POINT: Qty = Qty::parse_const("50");
    Measure::scaled(POINT)
}

fn es_net(book: &Book<Es>) -> Qty {
    book.net(&Es)
}

/// Long more than a million lots is a breach — never, here, but checked
/// every cycle.
fn es_assess(_: &mut Switch<Risk>, _: NanoTime, net: &Qty) -> Breaches<Risk> {
    let mut standing = Breaches::NONE;
    const MAX_LONG: Qty = Qty::parse_const("1000000");
    if *net > MAX_LONG {
        standing.set(Risk::MaxLong);
    }
    standing
}

wingfoil::nitro! {
    fn quoting(g: &GraphBuilder) -> Stream<Cycle> {
        let count = g.ticker(PERIOD).count();
        let clock = count.map(|_| ());
        let stamped = count.with_time();
        let desired = stamped.map(|(t, n): &(NanoTime, u64)| quote(*n, *t));
        let reports = stamped.map(|(t, n): &(NanoTime, u64)| venue_reports(*n, *t));
        let fills = reports.map(|r: &Burst<Report<Es>>| fills_of(r));
        let trading = count.map_filter(|_| (TradingState::Open, false));
        let pull = count.map_filter(|_| ((), false));
        let oms = reports.oms_op(
            &trading,
            &pull,
            &desired,
            &clock,
            config(),
            Epoch::ZERO,
            read_nothing,
        );
        let requests = oms.map(|(requests, _, _): &OmsOut| requests.clone());
        let executions = stamped.map(|(t, n): &(NanoTime, u64)| venue_executions(*n, *t));
        let bridge = executions.fix_bridge_op(&requests, false, Epoch::ZERO);
        let settlements = count.map_filter(|_| (Burst::<Fill<Es>>::new(), false));
        let cashflows = count.map_filter(|_| (Burst::<Cashflow<Es>>::new(), false));
        let net = fills.position(&settlements, &cashflows, es_measure, es_net);
        let readings = net.map(|q: &Qty| one(*q));
        let clear = count.map_filter(|_| ((), false));
        let switch = readings.kill_switch(&clear, Duration::ZERO, None, es_assess);
        let front = requests.join(
            &bridge,
            |r: &Burst<Request<Es>>, (m, b): &(Burst<Message<Es>>, Burst<Report<Es>>)| {
                (r.len(), m.len(), b.len())
            },
        );
        let out = front.join3(
            &net,
            &switch,
            |(r, m, b): &(usize, usize, usize), q: &Qty, s: &Assessed<Risk>| (*r, *m, *b, *q, *s),
        );
        out
    }
}

/// Every cycle placed one order and rendered one message, the bridge read
/// back the venue's ack and fill on the last one (and so forgot it), and the
/// fold holds every fill but the last cycle's order, which never filled.
fn check(last: Cycle) {
    let (requests, messages, translated, net, switch) = last;
    assert_eq!((requests, messages), (1, 1), "the OMS went idle");
    assert_eq!(
        translated, 2,
        "the bridge did not attribute the venue's answer"
    );
    let held = Qty::parse(&(CYCLES - 1).to_string()).expect("a whole lot count");
    assert_eq!(net, held);
    assert!(switch.standing.is_empty());
}

fn quoting_loop(c: &mut Criterion) {
    let run_for = RunFor::Cycles(CYCLES);

    // All three tiers must agree before being compared for speed.
    let (mut runner, out) = quoting::interpreted();
    runner.run(HISTORICAL, run_for).unwrap();
    let interp = runner.value(out);
    check(interp);
    let (compiled,) = quoting::compiled(HISTORICAL, run_for).unwrap();
    assert_eq!(interp, compiled);
    let g = GraphBuilder::new();
    let island = quoting::nested(&g);
    let mut r = g.build();
    r.run(HISTORICAL, run_for).unwrap();
    assert_eq!(interp, r.value(&island));

    let mut g = c.benchmark_group("execution_quoting");
    g.sample_size(30);
    g.throughput(Throughput::Elements(u64::from(CYCLES)));

    g.bench_function("interpreted", |b| {
        b.iter(|| {
            let (mut runner, out) = quoting::interpreted();
            runner.run(HISTORICAL, run_for).unwrap();
            black_box(runner.value(out))
        })
    });

    g.bench_function("compiled", |b| {
        b.iter(|| black_box(quoting::compiled(HISTORICAL, run_for).unwrap()))
    });

    g.bench_function("nested", |b| {
        b.iter(|| {
            let g = GraphBuilder::new();
            let island = quoting::nested(&g);
            let mut r = g.build();
            r.run(HISTORICAL, run_for).unwrap();
            black_box(r.value(&island))
        })
    });

    g.finish();
}

criterion_group!(benches, quoting_loop);
criterion_main!(benches);
