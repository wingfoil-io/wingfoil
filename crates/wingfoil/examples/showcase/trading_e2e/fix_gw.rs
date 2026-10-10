//! End-to-end trading demo — FIX gateway (wingfoil), on the execution layer.
//!
//! Two FIX sessions, both TLS to the LMAX London Demo:
//!
//! * **MD session** — `fix-marketdata.london-demo.lmax.com:443` (`LMXBDM`)
//!   subscribes to EUR/USD; folded into a top-of-book.
//! * **Order session** — `fix-order.london-demo.lmax.com:443` (`LMXBD`),
//!   wrapped as an execution-layer [`Venue`] (`lmax.rs`).
//!
//! Between the browser's clicks and the venue sits the execution layer
//! (`adapters::execution`), not a hand-rolled matcher:
//!
//! ```text
//!                       ┌──────────── reports ─────────────┬──────────────┐
//!                       ▼                                  │              ▼
//!   clicks ─► desk ─► OMS ─► requests ─╳─► ceiling ─► LMAX venue ──► position + kill switch
//!     ▲        │  ▲          (feedback)       (ReplaceChain, FIX)          │
//!     │        │  └──────────────── net, latch ◄───────────────────────────┤
//!     │        └─► answered clicks ─► iceoryx2 ─► ws_server                 └─► cancel-all ─► OMS
//! ```
//!
//! * **Desk** (`desk.rs`) — pre-trade checks (stale book, per-order size,
//!   position limit, kill switch), aggregation of the clicks waiting on a side
//!   into one OMS decision, and FIFO allocation of the fills back to clicks.
//! * **OMS** (`OmsOps::oms`) — turns each decision into requests: a cross is
//!   an immediate-or-cancel limit capped at the far touch, one request in
//!   flight per side, every request paid for from the order-rate budget.
//! * **The feedback cut** — the one place the loop is broken, on the request
//!   wire: the venue sees the OMS's burst one engine instant later.
//! * **Ceiling** (`ceiling::Capped`) — judges every order against the
//!   per-order cap before LMAX sees it, and *aborts the run* on a breach. The
//!   desk enforces the same number, so the ceiling is a backstop it never
//!   reaches.
//! * **Venue** (`lmax.rs`) — `fix::ReplaceChain` plus LMAX's tag=value codec.
//! * **Position and kill switch** — the position fold (`position::Book`)
//!   marked at mid, and a latch (`kill_switch::Switch`) over a position and a
//!   loss limit. A breach latches, pulls every working order through the
//!   OMS's cancel-all, and the desk refuses every click until restart.
//!
//! Stamps: `gw_recv` as a click arrives, `gw_price` as the desk accepts it,
//! `fix_send` as the desk learns which order carries it — the instant the
//! venue node injected that order — then `fix_recv` and `gw_publish` as the
//! answered click leaves for iceoryx2. The four stages on the WS edge live in
//! `ws_server`.
//!
//! # Run
//!
//! ```sh
//! LMAX_USERNAME=xxx LMAX_PASSWORD=yyy \
//!   cargo run -p wingfoil --release --example trading_e2e_fix_gw \
//!   --features "fix,iceoryx2,execution" -- [--no-precise]
//! ```
//!
//! Without `LMAX_USERNAME` / `LMAX_PASSWORD` the binary refuses to start — real
//! order routing requires real creds. (We deliberately removed the "simulated
//! fill" fallback so the latency report only ever shows honest end-to-end
//! numbers.)
//!
//! Limits, all optional: `WINGFOIL_MAX_MD_AGE_MS` (60000),
//! `WINGFOIL_MAX_ORDER_QTY` (10 contracts), `WINGFOIL_MAX_POSITION`
//! (50 contracts), `WINGFOIL_MAX_LOSS_USD` (1000).

#[path = "desk.rs"]
mod desk;
#[path = "lmax.rs"]
mod lmax;
#[path = "shared.rs"]
mod shared;

use std::cell::{Cell, RefCell};
use std::time::Duration;

use wingfoil::adapters::execution::ceiling::{Cap, Capped};
use wingfoil::adapters::execution::edge::{Report, Request};
use wingfoil::adapters::execution::kill_switch::{Breaches, Limit, Switch, within};
use wingfoil::adapters::execution::oms::{Config, Desired, Lifetime, OmsOps, Pacing, Passive};
use wingfoil::adapters::execution::order::{Epoch, Fill};
use wingfoil::adapters::execution::position::{Book, Measure};
use wingfoil::adapters::execution::rate_limit::{OrderRate, Terms};
use wingfoil::adapters::execution::venue::Venue;
use wingfoil::adapters::fix::{FixMessage, FixSessionStatus, fix_connect_tls};
use wingfoil::adapters::iceoryx2::{Iceoryx2SinkOps, iceoryx2_sub};
use wingfoil::adapters::market::{Px, Qty};
use wingfoil::latency::LatencyBurstStreamOps;
use wingfoil::prelude::*;
use wingfoil::{NanoTime, RunFor, RunMode};

use desk::{Click, Desk, Limits, RiskView, Touch};
use lmax::{EUR_USD_ID, EurUsd, Lmax};
use shared::{SVC_FILLS, SVC_ORDERS, env_u64, pin_current_from_env, round_trip_latency, stamping};

const LMAX_HOST_MD: &str = "fix-marketdata.london-demo.lmax.com";
const LMAX_HOST_ORD: &str = "fix-order.london-demo.lmax.com";
const LMAX_PORT: u16 = 443;
const LMAX_TARGET_MD: &str = "LMXBDM";
const LMAX_TARGET_ORD: &str = "LMXBD";

/// One LMAX EUR/USD contract is 10 000 euros, so a position's PnL is
/// `net × 10 000 × Δprice`, in dollars.
const CONTRACT_SIZE: &str = "10000";

// MarketData group (snapshot / incremental refresh).
const TAG_MD_ENTRY_TYPE: u32 = 269;
const TAG_MD_ENTRY_PX: u32 = 270;

/// This gateway's own order-entry throttle: 20 requests a second sustained,
/// 40 at once, and two cancel-alls, all spent at four fifths. Not LMAX's published limits — the
/// demo's, chosen well inside anything a demo account would be held to. A
/// click storm past it waits in the OMS, re-planned on every sweep, never
/// queued; the desk gives up on a click that waits longer than `max_wait`.
const RATE: OrderRate = match OrderRate::new(
    Terms {
        rate: 20,
        burst: 40,
    },
    Terms { rate: 2, burst: 2 },
    0.8,
) {
    Ok(rate) => rate,
    Err(_) => panic!("invariant: the gateway's order rate is valid"),
};

/// The ceiling on any one order, per contract.
#[derive(Clone, Copy, Debug)]
struct Ceiling(Qty);

impl Cap<EurUsd> for Ceiling {
    fn of(&self, _: &EurUsd) -> Option<Qty> {
        Some(self.0)
    }
}

/// What the kill switch watches.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
enum Risk {
    /// The net position past its limit. The desk's pre-trade check should
    /// make this unreachable; it is the backstop for when it is not.
    Position,
    /// The position's PnL past the loss limit — or not valued at all, which
    /// is a breach rather than a pass.
    Loss,
}

impl Limit for Risk {
    const ALL: &'static [Risk] = &[Risk::Position, Risk::Loss];
}

/// One instant's input to the desk, in the order the desk takes them.
#[derive(Clone, Debug, Default)]
enum DeskEvent {
    #[default]
    None,
    Sent(Burst<Request<EurUsd>>),
    Reports(Burst<Report<EurUsd>>),
    Risk(RiskView),
    Sweep,
    Touch(Touch),
    Clicks(Burst<Click>),
}

/// One instant's input to the risk node.
#[derive(Clone, Debug, Default)]
enum RiskEvent {
    #[default]
    None,
    Reports(Burst<Report<EurUsd>>),
    Touch(Touch),
}

fn main() -> anyhow::Result<()> {
    env_logger::init();
    rustls::crypto::ring::default_provider()
        .install_default()
        .ok();

    let stamping = stamping();
    let contracts = |name: &str, default: u64| -> anyhow::Result<Qty> {
        Qty::parse(&env_u64(name, default).to_string())
    };
    // LMAX London Demo updates EUR/USD only every few seconds during quiet
    // periods — observed gaps of 20+ seconds when the book is dormant — so
    // even 5 s rejects most orders outside of busy windows. 60 s keeps the
    // safety check meaningful while accepting the demo feed's real cadence.
    let limits = Limits {
        max_order: contracts("WINGFOIL_MAX_ORDER_QTY", 10)?,
        max_position: contracts("WINGFOIL_MAX_POSITION", 50)?,
        max_md_age: Duration::from_millis(env_u64("WINGFOIL_MAX_MD_AGE_MS", 60_000)),
        max_wait: Duration::from_secs(2),
    };
    let max_loss = env_u64("WINGFOIL_MAX_LOSS_USD", 1_000) as f64;

    let username = required_env("LMAX_USERNAME")?;
    let password = required_env("LMAX_PASSWORD")?;

    log::info!(
        "fix_gw starting — stamping={stamping:?} max_order={} max_position={} \
         max_md_age={:?} max_loss=${max_loss} as {username}",
        limits.max_order,
        limits.max_position,
        limits.max_md_age,
    );

    let g = GraphBuilder::new();

    // ── Two FIX sessions ─────────────────────────────────────────────────
    log::info!("connecting MD    session {LMAX_HOST_MD}:{LMAX_PORT} target={LMAX_TARGET_MD}");
    let fix_md = fix_connect_tls(
        &g,
        RunMode::RealTime,
        LMAX_HOST_MD,
        LMAX_PORT,
        &username,
        LMAX_TARGET_MD,
        Some(&password),
    )?;
    log::info!("connecting Order session {LMAX_HOST_ORD}:{LMAX_PORT} target={LMAX_TARGET_ORD}");
    let fix_ord = fix_connect_tls(
        &g,
        RunMode::RealTime,
        LMAX_HOST_ORD,
        LMAX_PORT,
        &username,
        LMAX_TARGET_ORD,
        Some(&password),
    )?;

    let touch = build_touch(&fix_md.data);
    let _md_sub = fix_md.fix_sub(g.constant(vec![EUR_USD_ID.to_string()]));
    let _md_status = log_status("md-session", &fix_md.status);
    let _ord_status = log_status("ord-session", &fix_ord.status);

    // Burst-shaped from the subscriber on: `iceoryx2_sub` drains everything
    // queued into one burst, and every click in it is an order.
    let clicks = iceoryx2_sub::<Click>(&g, RunMode::RealTime, SVC_ORDERS)?
        .inspect(|cs: &Burst<Click>| {
            for c in cs.iter() {
                log::info!(
                    "fix_gw: click seq={} qty={} side={}",
                    c.payload.client_seq,
                    c.payload.qty,
                    c.payload.side,
                );
            }
        })
        .stamp_each_as::<round_trip_latency::gw_recv>(stamping);

    // The clock the OMS re-plans on (staleness, a request the budget held
    // back) and the desk gives up on a click by.
    let sweep = g.ticker(Duration::from_millis(100));

    // ── The venue, behind the one feedback cut ──────────────────────────
    //
    // Requests come out of the OMS and reports go back in, so something
    // has to break the cycle; the execution layer puts the cut on the
    // request wire. `sent` is the OMS's previous-instant burst.
    // One epoch for the process, under which the OMS mints its order ids
    // and the replace chain its `ClOrdID`s: nothing is persisted here, so it
    // comes from the clock and need only differ from the last process's.
    let epoch = Epoch::from_clock(u64::from(NanoTime::now()) / 1_000_000_000);
    let (sent, cut) = g.feedback::<Burst<Request<EurUsd>>>();
    let lmax = Lmax {
        inbound: fix_ord.data.clone(),
        sender: fix_ord.sender(),
        epoch,
    };
    let session = Capped::new(&lmax, Ceiling(limits.max_order)).wire(&sent);

    // ── Position and the kill switch ─────────────────────────────────────
    let risk = build_risk(&g, &session.reports, &touch, limits.max_position, max_loss);
    // The latch's rising edge pulls everything the OMS has working.
    let cancel_all = {
        let was = Cell::new(false);
        risk.filter_map(move |r: &RiskView| {
            let rose = r.halted && !was.replace(r.halted);
            rose.then_some(())
        })
    };

    // ── The desk ─────────────────────────────────────────────────────────
    let desk_out = {
        let desk = RefCell::new(Desk::new(limits, stamping));
        let events = [
            sent.map(|r: &Burst<Request<EurUsd>>| DeskEvent::Sent(r.clone())),
            session
                .reports
                .map(|r: &Burst<Report<EurUsd>>| DeskEvent::Reports(r.clone())),
            risk.map(|r: &RiskView| DeskEvent::Risk(*r)),
            sweep.map(|_: &()| DeskEvent::Sweep),
            touch.map(|t: &Touch| DeskEvent::Touch(*t)),
            clicks.map(|c: &Burst<Click>| DeskEvent::Clicks(c.clone())),
        ];
        // `combine` hands over everything that ticked this instant, in the
        // order above: what was sent before what it was answered with, and
        // the book before the clicks priced against it.
        g.combine(&events)
            .with_time()
            .map(move |(now, events): &(NanoTime, Burst<DeskEvent>)| {
                let mut desk = desk.borrow_mut();
                let mut answered: Burst<Click> = Burst::new();
                for event in events {
                    match event {
                        DeskEvent::Sent(requests) => desk.sent(requests),
                        DeskEvent::Reports(reports) => desk.reported(reports, &mut answered),
                        DeskEvent::Risk(risk) => desk.risk(*risk, &mut answered),
                        DeskEvent::Sweep => desk.sweep(*now, &mut answered),
                        DeskEvent::Touch(touch) => desk.touch(*touch),
                        DeskEvent::Clicks(clicks) => {
                            for click in clicks {
                                desk.click(*now, *click, &mut answered);
                            }
                        }
                        DeskEvent::None => {}
                    }
                }
                let desired: Burst<Desired<EurUsd>> = desk.decide(*now).into_iter().collect();
                (desired, answered)
            })
    };
    let desired = desk_out.filter_map(|(d, _): &(Burst<Desired<EurUsd>>, Burst<Click>)| {
        (!d.is_empty()).then(|| d.clone())
    });

    // ── The OMS ──────────────────────────────────────────────────────────
    let config = Config {
        // A cross is remembered while its side is busy; the desk gives up
        // on a click well before this withdraws the decision under it.
        max_desired_age: Duration::from_secs(5),
        min_requote: Px::ZERO,
        // The desk decides on clicks, never on a report, so a killed IOC
        // does not wake a fresh cross and there is no loop to space out.
        retake: Duration::ZERO,
        rate: RATE,
        // LMAX has no post-only order type.
        passive: Passive::Limit,
        ratio: None,
        lifetime: Lifetime::GoodTillCancel,
    };
    let (requests, pacing) = desired.oms(
        config,
        epoch,
        &session.reports,
        &session.trading,
        &cancel_all,
        &sweep,
    );
    let _cut = requests.feedback(&cut);
    let _pacing = log_pacing(&pacing);

    // ── Answered clicks → iceoryx2 ──────────────────────────────────────
    let _pub_fills = desk_out
        .filter_map(|(_, a): &(Burst<Desired<EurUsd>>, Burst<Click>)| {
            (!a.is_empty()).then(|| a.clone())
        })
        .stamp_each_all::<(round_trip_latency::fix_recv, round_trip_latency::gw_publish)>(stamping)
        .inspect(|cs: &Burst<Click>| {
            for c in cs.iter() {
                log::info!(
                    "fix_gw: answering seq={} filled_qty={} px_bps={}",
                    c.payload.client_seq,
                    c.payload.filled_qty,
                    c.payload.fill_price_bps,
                );
            }
        })
        .iceoryx2_pub(SVC_FILLS);

    // Pin AFTER all eagerly-spawned adapter workers are running so they keep
    // the default affinity mask. The pinned mask applies only to the graph
    // cycle thread (this one).
    pin_current_from_env("WINGFOIL_PIN_GRAPH");

    g.build().run(RunMode::RealTime, RunFor::Forever)?;
    Ok(())
}

/// Read an env var that must be set to a non-empty value. `std::env::var`
/// returns `Ok("")` when the var is set but empty, which would otherwise
/// slip past a plain `?` check and produce confusing FIX login failures.
fn required_env(name: &str) -> anyhow::Result<String> {
    match std::env::var(name) {
        Ok(v) if !v.trim().is_empty() => Ok(v),
        _ => anyhow::bail!("{name} env var is required (and must be non-empty)"),
    }
}

// ── Top of book from the LMAX MD stream ──────────────────────────────────
//
// LMAX sends MarketDataSnapshotFullRefresh (MsgType W) and
// MarketDataIncrementalRefresh (X) with repeating groups of
// (269 MDEntryType, 270 MDEntryPx, 271 MDEntrySize). We don't need the
// size, so walk the fields linearly: a 269 sets the side the next 270 is
// for — bid (269=0) or offer (269=1).
//
// The whole burst is folded: a cycle carrying a bid refresh and then an
// offer refresh must apply both.
fn build_touch(data: &Stream<Burst<FixMessage>>) -> Stream<Touch> {
    data.with_time().fold(
        Touch::default(),
        |touch: &mut Touch, (now, msgs): &(NanoTime, Burst<FixMessage>)| {
            for msg in msgs.iter() {
                if !matches!(msg.msg_type.as_str(), "W" | "X") {
                    continue;
                }
                let mut entry_type: Option<u8> = None;
                for (tag, val) in &msg.fields {
                    match *tag {
                        TAG_MD_ENTRY_TYPE => entry_type = val.parse::<u8>().ok(),
                        TAG_MD_ENTRY_PX => {
                            if let (Some(t), Ok(px)) = (entry_type, Px::parse(val)) {
                                match t {
                                    0 => touch.bid = Some(px),
                                    1 => touch.ask = Some(px),
                                    _ => {}
                                }
                                touch.at = *now;
                            }
                        }
                        _ => {}
                    }
                }
            }
        },
    )
}

// ── Position and kill switch ─────────────────────────────────────────────

/// The position fold over every execution, marked at mid, and the latch
/// over it — reassessed whenever either moves.
fn build_risk(
    g: &GraphBuilder,
    reports: &Stream<Burst<Report<EurUsd>>>,
    touch: &Stream<Touch>,
    max_position: Qty,
    max_loss: f64,
) -> Stream<RiskView> {
    let size = Qty::parse(CONTRACT_SIZE).expect("invariant: the contract size parses");
    let book = RefCell::new(Book::new(move |_: &EurUsd| Measure::scaled(size)));
    let switch = RefCell::new(Switch::<Risk>::new(Duration::ZERO));
    let mark: Cell<Option<Px>> = Cell::new(None);
    let events = [
        reports.map(|r: &Burst<Report<EurUsd>>| RiskEvent::Reports(r.clone())),
        touch.map(|t: &Touch| RiskEvent::Touch(*t)),
    ];
    g.combine(&events)
        .with_time()
        .map(move |(now, events): &(NanoTime, Burst<RiskEvent>)| {
            let mut book = book.borrow_mut();
            let mut traded = false;
            for event in events {
                match event {
                    RiskEvent::Reports(reports) => {
                        let fills: Vec<Fill<EurUsd>> = reports
                            .iter()
                            .filter_map(|r| match r {
                                Report::Fill(fill) => Some(*fill),
                                _ => None,
                            })
                            .collect();
                        traded |= !fills.is_empty();
                        book.apply(&fills);
                    }
                    RiskEvent::Touch(touch) => mark.set(touch.mid().or(mark.get())),
                    RiskEvent::None => {}
                }
            }
            let net = book.net(&EurUsd);
            let pnl = match (book.position(&EurUsd), mark.get()) {
                (Some(position), Some(mark)) => position.pnl(mark),
                _ => None,
            };
            let mut standing = Breaches::NONE;
            if !within(net.abs().to_f64(), max_position.to_f64()) {
                standing.set(Risk::Position);
            }
            // Flat with no mark has nothing to value; a position with no
            // value is a breach, never a pass.
            let loss = match pnl {
                Some(pnl) => -pnl.net().to_f64(),
                None if net.is_zero() => 0.0,
                None => f64::NAN,
            };
            if !within(loss, max_loss) {
                standing.set(Risk::Loss);
            }
            let mut switch = switch.borrow_mut();
            let was = switch.latch().is_some();
            switch.record(*now, standing);
            if traded && let Some(pnl) = pnl {
                log::info!(
                    "position: net={net} realised=${} unrealised=${}",
                    pnl.realised,
                    pnl.unrealised
                );
            }
            if !was && let Some(latch) = switch.latch() {
                log::error!(
                    "kill switch latched on {:?} (net={net}, loss=${loss:.2}) — \
                     pulling every order; restart to clear",
                    latch.breaches
                );
            }
            RiskView {
                net,
                halted: switch.latch().is_some(),
            }
        })
}

fn log_pacing(pacing: &Stream<Pacing>) -> Stream<()> {
    let last = Cell::new(Pacing::default());
    pacing.for_each(move |p: &Pacing| {
        if (p.deferred, p.superseded) != (last.get().deferred, last.get().superseded) {
            log::warn!(
                "oms: order-rate budget — {} deferred, {} superseded, {} waiting",
                p.deferred,
                p.superseded,
                p.waiting
            );
        }
        last.set(*p);
        Ok(())
    })
}

fn log_status(label: &'static str, status: &Stream<Burst<FixSessionStatus>>) -> Stream<()> {
    let cell: RefCell<FixSessionStatus> = RefCell::new(FixSessionStatus::Disconnected);
    status.for_each(move |burst: &Burst<FixSessionStatus>| {
        for st in burst.iter() {
            if *st != *cell.borrow() {
                log::info!("{label}: {st:?}");
                *cell.borrow_mut() = st.clone();
            }
        }
        Ok(())
    })
}
