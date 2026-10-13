//! End-to-end trading demo — FIX gateway (wingfoil), on the execution layer.
//!
//! Two FIX sessions, both TLS to the LMAX London Demo:
//!
//! * **MD session** — `fix-marketdata.london-demo.lmax.com:443` (`LMXBDM`)
//!   subscribes to EUR/USD; folded into a top-of-book.
//! * **Order session** — `fix-order.london-demo.lmax.com:443` (`LMXBD`),
//!   the library's FIX venue (`fix::order_entry::FixOrderVenue`) in LMAX's
//!   dialect (`lmax.rs`).
//!
//! Each browser click is one order: an immediate-or-cancel limit capped at
//! the far touch, sent on the execution layer's order edge and answered
//! from its reports. No order manager — a click is an order, not a state to
//! work towards — and no FIX of our own.
//!
//! ```text
//!   clicks ─► router ─► requests ─╳─► ceiling ─► FixOrderVenue ──┬─► reports ─► router ─► answered clicks
//!               ▲                (feedback cut)    (LMAX dialect)  │
//!               └──────────── net, latch ◄── position + kill switch ◄┘
//! ```
//!
//! * **Router** — pre-trade checks (stale book, per-order size, kill switch,
//!   the worst-case position), one `Request::Place` per click under its own
//!   `ClientOrderId`, and the click answered when its order ends.
//! * **The feedback cut** — the one place the request → report loop is
//!   broken: the venue sees a click's order one engine instant later.
//! * **Ceiling** (`ceiling::Capped`) — judges every order against the
//!   per-order cap before LMAX sees it, and aborts the run on a breach; the
//!   router refuses the same sizes first.
//! * **Venue** — the replace chain, the FIX 4.4 codec and the send, all the
//!   library's. `lmax.rs` says only how LMAX names EUR/USD.
//! * **Position and kill switch** — the position fold (`position::Book`)
//!   marked at mid, and a latch (`kill_switch::Switch`) over a position and a
//!   loss limit. Once latched, every click is refused until restart; an IOC
//!   never rests, so there is nothing at the venue to pull.
//!
//! Stamps: `gw_recv` as a click arrives, `gw_price` and `fix_send` as its
//! order is emitted, then `fix_recv` and `gw_publish` as the answered click
//! leaves for iceoryx2. The four stages on the WS edge live in `ws_server`.
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

#[path = "lmax.rs"]
mod lmax;
#[path = "shared.rs"]
mod shared;

use std::cell::{Cell, RefCell};
use std::collections::BTreeMap;
use std::time::Duration;

use wingfoil::adapters::execution::ceiling::{Cap, Capped};
use wingfoil::adapters::execution::edge::{Report, Request};
use wingfoil::adapters::execution::kill_switch::{Breaches, Limit, Switch, within};
use wingfoil::adapters::execution::order::{ClientOrderId, Epoch, Fill, Order, TimeInForce};
use wingfoil::adapters::execution::position::{Book, Measure};
use wingfoil::adapters::execution::venue::Venue;
use wingfoil::adapters::fix::order_entry::{FixCodec, FixOrderVenue};
use wingfoil::adapters::fix::{FixMessage, FixSessionStatus, fix_connect_tls};
use wingfoil::adapters::iceoryx2::{Iceoryx2SinkOps, iceoryx2_sub};
use wingfoil::adapters::market::{Amount, Px, Qty, SCALE, Side, mul_div};
use wingfoil::latency::{LatencyBurstStreamOps, Stage, Traced};
use wingfoil::prelude::*;
use wingfoil::{NanoTime, RunFor, RunMode};

use lmax::{EUR_USD_ID, EurUsd, Lmax};
use shared::{
    RoundTrip, RoundTripLatency, SIDE_BUY, SVC_FILLS, SVC_ORDERS, env_u64, pin_current_from_env,
    round_trip_latency, stamping,
};

const LMAX_HOST_MD: &str = "fix-marketdata.london-demo.lmax.com";
const LMAX_HOST_ORD: &str = "fix-order.london-demo.lmax.com";
const LMAX_PORT: u16 = 443;
const LMAX_TARGET_MD: &str = "LMXBDM";
const LMAX_TARGET_ORD: &str = "LMXBD";

/// One LMAX EUR/USD contract is 10 000 euros, so a position's PnL is
/// `net × 10 000 × Δprice`, in dollars.
const CONTRACT_SIZE: i128 = 10_000;

/// An IOC ends in milliseconds; a click whose order has not ended in this
/// long has lost its report, and is answered with what it got.
const MAX_WORKING: Duration = Duration::from_secs(10);

// MarketData group (snapshot / incremental refresh).
const TAG_MD_ENTRY_TYPE: u32 = 269;
const TAG_MD_ENTRY_PX: u32 = 270;

/// One browser order, with its latency record.
type Click = Traced<RoundTrip, RoundTripLatency>;

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
    /// The net position past its limit — the backstop to the router's
    /// pre-trade check.
    Position,
    /// The position's loss past its limit — or not valued at all, which is
    /// a breach rather than a pass.
    Loss,
}

impl Limit for Risk {
    const ALL: &'static [Risk] = &[Risk::Position, Risk::Loss];
}

/// The touch, as the market-data session last stated it.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
struct Touch {
    bid: Option<Px>,
    ask: Option<Px>,
    /// Engine time of the last update.
    at: NanoTime,
}

/// What the risk node last said.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
struct RiskView {
    net: Qty,
    halted: bool,
}

#[derive(Clone, Copy, Debug)]
struct Limits {
    max_order: Qty,
    max_position: Qty,
    max_md_age: Duration,
}

/// One instant's input to the router, in the order it takes them: what the
/// venue said, then what the next clicks are judged against, then the
/// clicks.
#[derive(Clone, Debug, Default)]
enum Event {
    #[default]
    None,
    Reports(Burst<Report<EurUsd>>),
    Risk(RiskView),
    Touch(Touch),
    Sweep,
    Clicks(Burst<Click>),
}

/// A click awaiting its order's end.
struct InFlight {
    /// `None` once answered: an order given up on stays counted until it
    /// really ends, since it may yet fill.
    click: Option<Click>,
    side: Side,
    qty: Qty,
    filled: Qty,
    /// Σ qty × price over its executions, exact.
    notional: Amount,
    sent: NanoTime,
}

/// Clicks in, one order each; reports in, clicks answered.
struct Router {
    limits: Limits,
    stamping: bool,
    epoch: Epoch,
    next: u32,
    touch: Touch,
    risk: RiskView,
    /// Ordered, so a sweep answers in the same order every run.
    in_flight: BTreeMap<ClientOrderId, InFlight>,
}

impl Router {
    fn reported(&mut self, reports: &[Report<EurUsd>], out: &mut Burst<Click>) {
        for report in reports {
            let (order, ended) = match report {
                Report::Fill(fill) => {
                    if let Some(f) = self.in_flight.get_mut(&fill.order) {
                        f.filled = f.filled + fill.qty;
                        if let Some(notional) = Amount::of(fill.qty, fill.price) {
                            f.notional = f.notional + notional;
                        }
                    }
                    (fill.order, fill.remaining <= Qty::ZERO)
                }
                Report::Cancelled(r) | Report::Expired(r) => (r.order, true),
                // Nothing here sends a cancel or a replace, so a refusal is
                // the order's own: it never lived.
                Report::Reject(r) => match r.order {
                    Some(order) => (order, true),
                    None => continue,
                },
                Report::Ack(_) | Report::Triggered(_) => continue,
            };
            if ended && let Some(mut f) = self.in_flight.remove(&order) {
                out.extend(answer(&mut f));
            }
        }
    }

    /// Answer clicks whose order has gone `MAX_WORKING` without ending. The
    /// order stays in flight — still counted against the position — until
    /// its end is reported, since it may yet fill.
    fn sweep(&mut self, now: NanoTime, out: &mut Burst<Click>) {
        for (id, f) in &mut self.in_flight {
            let overdue = now >= f.sent && Duration::from(now - f.sent) >= MAX_WORKING;
            if overdue && f.click.is_some() {
                log::error!("router: order {id:?} has not ended in {MAX_WORKING:?}");
                out.extend(answer(f));
            }
        }
    }

    /// One click: refused at once, or sent as its own IOC.
    fn click(
        &mut self,
        now: NanoTime,
        mut click: Click,
        requests: &mut Burst<Request<EurUsd>>,
        out: &mut Burst<Click>,
    ) {
        let side = if click.payload.side == SIDE_BUY {
            Side::Bid
        } else {
            Side::Ask
        };
        let qty = Qty::from_raw(i128::from(click.payload.qty) * SCALE);
        // A buy is capped at the ask, a sell at the bid.
        let cap = match side {
            Side::Bid => self.touch.ask,
            Side::Ask => self.touch.bid,
        };
        // Both sides: the order is capped at one, and the position it makes
        // is valued at the mid of both — a fill the risk node cannot value
        // would latch the kill switch.
        let fresh = self.touch.bid.is_some()
            && self.touch.ask.is_some()
            && now >= self.touch.at
            && Duration::from(now - self.touch.at) <= self.limits.max_md_age;
        // The worst case on the click's own side: every order that way
        // filling, nothing the other way. Netting the two would let a buy in
        // on the strength of a sell that may yet be killed unfilled.
        let mut worst = self.risk.net + signed(side, qty);
        for f in self.in_flight.values().filter(|f| f.side == side) {
            worst = worst + signed(side, f.qty - f.filled);
        }
        let refused = if qty <= Qty::ZERO || qty > self.limits.max_order {
            Some("size outside the per-order limit")
        } else if self.risk.halted {
            Some("kill switch latched")
        } else if !fresh {
            Some("book stale or one-sided")
        } else if worst.abs() > self.limits.max_position {
            Some("position limit")
        } else {
            None
        };
        let (Some(cap), None) = (cap, refused) else {
            let why = refused.unwrap_or("book stale or one-sided");
            log::warn!(
                "router: click seq={} refused — {why}",
                click.payload.client_seq
            );
            out.extend(answer(&mut in_flight(click, side, qty, now)));
            return;
        };
        let id = ClientOrderId::new(self.epoch, self.next);
        self.next += 1;
        requests.push(Request::Place(Order {
            tif: TimeInForce::ImmediateOrCancel,
            ..Order::limit(id, EurUsd, side, qty, cap)
        }));
        if self.stamping {
            let at = u64::from(NanoTime::now());
            round_trip_latency::gw_price::stamp(&mut click.latency, at);
            round_trip_latency::fix_send::stamp(&mut click.latency, at);
        }
        log::info!(
            "router: click seq={} → order {} {side:?} {qty} capped at {cap}",
            click.payload.client_seq,
            id.sequence()
        );
        self.in_flight.insert(id, in_flight(click, side, qty, now));
    }
}

fn in_flight(click: Click, side: Side, qty: Qty, sent: NanoTime) -> InFlight {
    InFlight {
        click: Some(click),
        side,
        qty,
        filled: Qty::ZERO,
        notional: Amount::ZERO,
        sent,
    }
}

fn signed(side: Side, qty: Qty) -> Qty {
    match side {
        Side::Bid => qty,
        Side::Ask => -qty,
    }
}

/// A click's answer: what it got, at its average price in the wire's basis
/// points, rounded to nearest. Zero for a refused or unfilled click. `None`
/// if it was answered already.
fn answer(f: &mut InFlight) -> Option<Click> {
    let mut click = f.click.take()?;
    click.payload.filled_qty = u64::try_from(f.filled.raw() / SCALE).unwrap_or(0);
    click.payload.fill_price_bps = f
        .notional
        .per(f.filled)
        .and_then(|avg| mul_div(avg.raw(), 20_000, SCALE))
        .and_then(|twice| i64::try_from((twice + 1) / 2).ok())
        .unwrap_or(0);
    Some(click)
}

fn main() -> anyhow::Result<()> {
    env_logger::init();
    rustls::crypto::ring::default_provider()
        .install_default()
        .ok();

    let stamping = stamping();
    let contracts =
        |name: &str, default: u64| Qty::from_raw(i128::from(env_u64(name, default)) * SCALE);
    // LMAX London Demo updates EUR/USD only every few seconds during quiet
    // periods — observed gaps of 20+ seconds when the book is dormant — so
    // even 5 s rejects most orders outside of busy windows. 60 s keeps the
    // safety check meaningful while accepting the demo feed's real cadence.
    let limits = Limits {
        max_order: contracts("WINGFOIL_MAX_ORDER_QTY", 10),
        max_position: contracts("WINGFOIL_MAX_POSITION", 50),
        max_md_age: Duration::from_millis(env_u64("WINGFOIL_MAX_MD_AGE_MS", 60_000)),
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
        .stamp_each_as::<round_trip_latency::gw_recv>(stamping);

    // ── The venue, behind the one feedback loop ─────────────────────────
    //
    // Order ids — and the `ClOrdID`s the replace chain mints under the same
    // epoch — carry an epoch from the clock, so a restarted gateway does not
    // send one LMAX has seen from the last process.
    let epoch = Epoch::from_clock(u64::from(NanoTime::now()) / 1_000_000_000);
    // What the router sends round, and what it answers: requests go out to
    // the venue, clicks back to the browser.
    type Routed = (Burst<Request<EurUsd>>, Burst<Click>);
    // `sent` is what the router sent, one instant late: the loop's cut.
    let FeedbackLoop { out: routed, .. } = g.feedback_loop(|sent| {
        let lmax = FixOrderVenue {
            inbound: fix_ord.data.clone(),
            sender: fix_ord.sender(),
            epoch,
            mass_cancel: false,
            codec: FixCodec::new(Lmax),
        };
        let session = Capped::new(&lmax, Ceiling(limits.max_order)).wire(sent);

        // ── Position and the kill switch ─────────────────────────────────
        let risk = build_risk(&g, &session.reports, &touch, limits.max_position, max_loss);

        // ── The router ───────────────────────────────────────────────────
        let routed = {
            let router = RefCell::new(Router {
                limits,
                stamping: stamping.is_on(),
                epoch,
                next: 1,
                touch: Touch::default(),
                risk: RiskView::default(),
                in_flight: BTreeMap::new(),
            });
            let events = [
                session
                    .reports
                    .map(|r: &Burst<Report<EurUsd>>| Event::Reports(r.clone())),
                risk.map(|r: &RiskView| Event::Risk(*r)),
                touch.map(|t: &Touch| Event::Touch(*t)),
                g.ticker(Duration::from_secs(1)).map(|_: &()| Event::Sweep),
                clicks.map(|c: &Burst<Click>| Event::Clicks(c.clone())),
            ];
            g.combine(&events)
                .with_time()
                .map(move |(now, events): &(NanoTime, Burst<Event>)| {
                    let mut router = router.borrow_mut();
                    let (mut requests, mut answered): Routed = Default::default();
                    for event in events {
                        match event {
                            Event::Reports(reports) => router.reported(reports, &mut answered),
                            Event::Risk(risk) => router.risk = *risk,
                            Event::Touch(touch) => router.touch = *touch,
                            Event::Sweep => router.sweep(*now, &mut answered),
                            Event::Clicks(clicks) => {
                                for click in clicks {
                                    router.click(*now, *click, &mut requests, &mut answered);
                                }
                            }
                            Event::None => {}
                        }
                    }
                    (requests, answered)
                })
        };
        let requests = routed.filter_map(|(r, _): &Routed| (!r.is_empty()).then(|| r.clone()));
        FeedbackLoop {
            feedback: requests,
            out: routed,
        }
    });

    // ── Answered clicks → iceoryx2 ──────────────────────────────────────
    let _pub_fills = routed
        .filter_map(|(_, a): &Routed| (!a.is_empty()).then(|| a.clone()))
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

/// One instant's input to the risk node.
#[derive(Clone, Debug, Default)]
enum RiskEvent {
    #[default]
    None,
    Reports(Burst<Report<EurUsd>>),
    Touch(Touch),
}

/// The position fold over every execution, marked at mid, and the latch over
/// it — reassessed whenever either moves. Latched, it stays latched until
/// restart: clearing a kill switch is an operator's.
fn build_risk(
    g: &GraphBuilder,
    reports: &Stream<Burst<Report<EurUsd>>>,
    touch: &Stream<Touch>,
    max_position: Qty,
    max_loss: f64,
) -> Stream<RiskView> {
    let size = Qty::from_raw(CONTRACT_SIZE * SCALE);
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
                        book.apply(&fills);
                    }
                    RiskEvent::Touch(t) => {
                        if let (Some(bid), Some(ask)) = (t.bid, t.ask) {
                            mark.set(Some(Px::midpoint(bid, ask)));
                        }
                    }
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
            // A profit is no loss: `within` compares magnitudes. A position
            // that cannot be valued is a breach (NaN), never a pass.
            let loss = match pnl {
                Some(pnl) => (-pnl.net().to_f64()).max(0.0),
                None if net.is_zero() => 0.0,
                None => f64::NAN,
            };
            if !within(loss, max_loss) {
                standing.set(Risk::Loss);
            }
            let mut switch = switch.borrow_mut();
            let was = switch.latch().is_some();
            switch.record(*now, standing);
            if !was && let Some(latch) = switch.latch() {
                log::error!(
                    "kill switch latched on {:?} (net={net}, loss=${loss:.2}) — \
                     refusing every click until restart",
                    latch.breaches
                );
            }
            RiskView {
                net,
                halted: switch.latch().is_some(),
            }
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

#[cfg(test)]
mod tests {
    use super::*;
    use wingfoil::adapters::execution::edge::Retired;
    use wingfoil::adapters::execution::exec_id::ExecId;
    use wingfoil::adapters::market::{px, qty};

    fn t(ms: u64) -> NanoTime {
        NanoTime::new(1_000_000_000 + ms * 1_000_000)
    }

    fn router() -> Router {
        Router {
            limits: Limits {
                max_order: qty!(10),
                max_position: qty!(20),
                max_md_age: Duration::from_secs(60),
            },
            stamping: false,
            epoch: Epoch::ZERO,
            next: 1,
            touch: Touch {
                bid: Some(px!(1.0850)),
                ask: Some(px!(1.0852)),
                at: t(0),
            },
            risk: RiskView::default(),
            in_flight: BTreeMap::new(),
        }
    }

    fn click(seq: u64, side: u8, qty: u64) -> Click {
        Click::new(RoundTrip {
            client_seq: seq,
            side,
            qty,
            ..RoundTrip::default()
        })
    }

    fn send(router: &mut Router, at: NanoTime, c: Click) -> (Burst<Request<EurUsd>>, Burst<Click>) {
        let (mut requests, mut out) = (Burst::new(), Burst::new());
        router.click(at, c, &mut requests, &mut out);
        (requests, out)
    }

    /// A buy is its own IOC, capped at the ask; its fills come back to it,
    /// and it is answered when the order ends, at its average price.
    #[test]
    fn a_click_is_one_ioc_and_is_answered_when_it_ends() {
        let mut router = router();
        let (requests, out) = send(&mut router, t(1), click(7, SIDE_BUY, 3));
        assert!(out.is_empty());
        let [Request::Place(order)] = requests.as_slice() else {
            panic!("{requests:?}")
        };
        assert_eq!(
            (order.side, order.qty, order.price, order.tif),
            (
                Side::Bid,
                qty!(3),
                Some(px!(1.0852)),
                TimeInForce::ImmediateOrCancel
            )
        );
        let mut out = Burst::new();
        router.reported(
            &[Report::Fill(Fill {
                order: order.id,
                exec_id: ExecId::new("x").unwrap(),
                instrument: EurUsd,
                side: Side::Bid,
                qty: qty!(2),
                price: px!(1.08515),
                remaining: qty!(1),
                ..Default::default()
            })],
            &mut out,
        );
        assert!(out.is_empty(), "the IOC has not ended");
        router.reported(
            &[Report::Cancelled(Retired {
                order: order.id,
                remaining: qty!(1),
                ..Default::default()
            })],
            &mut out,
        );
        assert_eq!(out.len(), 1);
        assert_eq!(
            (
                out[0].payload.client_seq,
                out[0].payload.filled_qty,
                out[0].payload.fill_price_bps
            ),
            (7, 2, 10852)
        );
    }

    /// A buy in flight counts against the next buy; a sell in flight does
    /// not make room for one.
    #[test]
    fn the_position_check_takes_the_worst_case_on_the_clicks_side() {
        let mut router = router();
        router.risk.net = qty!(8);
        send(&mut router, t(1), click(1, SIDE_BUY, 10));
        send(&mut router, t(1), click(2, shared::SIDE_SELL, 10));
        let (requests, out) = send(&mut router, t(1), click(3, SIDE_BUY, 3));
        assert!(requests.is_empty());
        assert_eq!(
            out.len(),
            1,
            "8 + 10 + 3 is past 20, whatever the sell does"
        );
    }

    /// Refused at once, with nothing sent: a latched switch, a stale book.
    #[test]
    fn a_latch_or_a_stale_book_refuses_the_click() {
        let mut router = router();
        router.risk.halted = true;
        assert!(send(&mut router, t(1), click(1, SIDE_BUY, 1)).0.is_empty());
        router.risk.halted = false;
        assert!(
            send(&mut router, t(61_000), click(2, SIDE_BUY, 1))
                .0
                .is_empty()
        );
        assert!(!send(&mut router, t(1), click(3, SIDE_BUY, 1)).0.is_empty());
    }

    /// An order whose end never comes has its click answered after
    /// `MAX_WORKING`.
    #[test]
    fn a_lost_order_is_answered_after_max_working() {
        let mut router = router();
        send(&mut router, t(1), click(1, SIDE_BUY, 1));
        let mut out = Burst::new();
        router.sweep(t(9_000), &mut out);
        assert!(out.is_empty());
        router.sweep(t(10_001), &mut out);
        assert_eq!(out.len(), 1);
        router.sweep(t(20_000), &mut out);
        assert_eq!(out.len(), 1, "answered once");
        // Still counted: 18 + its 1 + 2 is past 20.
        router.risk.net = qty!(18);
        assert!(
            send(&mut router, t(20_001), click(2, SIDE_BUY, 2))
                .0
                .is_empty()
        );
    }

    /// With one side of the book unknown, nothing is sent: the position it
    /// made could not be valued, and the kill switch would latch on it.
    #[test]
    fn a_one_sided_book_refuses_the_click() {
        let mut router = router();
        router.touch.bid = None;
        let (requests, out) = send(&mut router, t(1), click(1, SIDE_BUY, 1));
        assert!(requests.is_empty());
        assert_eq!(out.len(), 1);
    }
}
