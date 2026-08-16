//! End-to-end latency demo — FIX gateway (wingfoil).
//!
//! Two FIX sessions, both TLS to the LMAX London Demo:
//!
//! * **MD session** — `fix-marketdata.london-demo.lmax.com:443` (`LMXBDM`)
//!   subscribes to EUR/USD; folded into a top-of-book.
//! * **Order session** — `fix-order.london-demo.lmax.com:443` (`LMXBD`)
//!   receives `NewOrderSingle` injections and surfaces `ExecutionReport`s.
//!
//! Pipeline (this binary):
//!
//! ```text
//!   ws_server ── iceoryx2 ──► fix_gw ──── FIX/TLS ──► LMAX                (orders)
//!   ws_server ◄── iceoryx2 ── fix_gw ◄─── FIX/TLS ─── LMAX                (fills)
//! ```
//!
//! Stamps `gw_recv` and `gw_price` on the way out, `fix_send` just before FIX
//! injection, then on the inbound side `fix_recv` (when the `ExecutionReport`
//! surfaces) and `gw_publish` (just before iceoryx2 publish). The four stages on
//! the WS edge live in `ws_server`.
//!
//! A port of the legacy `legacy/wingfoil/examples/latency_e2e/fix_gw.rs` onto the wingfoil
//! engine. The pipeline shape, the stamp stages and the matcher semantics are
//! unchanged; only the wiring is wingfoil-idiomatic — a `GraphBuilder` plus the
//! adapter extension traits, `join_passive` in place of
//! `bimap(Dep::Active, Dep::Passive)`, and `map_filter` in place of
//! `filter_map`.
//!
//! # Run
//!
//! ```sh
//! LMAX_USERNAME=xxx LMAX_PASSWORD=yyy \
//!   cargo run -p wingfoil --release --example trading_e2e_fix_gw \
//!   --features "fix,iceoryx2" -- [--no-precise]
//! ```
//!
//! Without `LMAX_USERNAME` / `LMAX_PASSWORD` the binary refuses to start — real
//! order routing requires real creds. (We deliberately removed the "simulated
//! fill" fallback so the latency report only ever shows honest end-to-end
//! numbers.)

#[path = "shared.rs"]
mod shared;

use std::cell::RefCell;
use std::collections::HashMap;

use wingfoil::adapters::fix::{FixMessage, FixSender, FixSessionStatus, fix_connect_tls};
use wingfoil::adapters::iceoryx2::{Iceoryx2SinkOps, iceoryx2_sub};
use wingfoil::latency::{LatencyBurstStreamOps, Traced};
use wingfoil::prelude::*;
use wingfoil::{NanoTime, RunFor, RunMode};

use shared::{
    RoundTrip, RoundTripLatency, SIDE_BUY, SVC_FILLS, SVC_ORDERS, env_u64, pin_current_from_env,
    round_trip_latency, session_hex, stamping,
};

const LMAX_HOST_MD: &str = "fix-marketdata.london-demo.lmax.com";
const LMAX_HOST_ORD: &str = "fix-order.london-demo.lmax.com";
const LMAX_PORT: u16 = 443;
const LMAX_TARGET_MD: &str = "LMXBDM";
const LMAX_TARGET_ORD: &str = "LMXBD";
const EUR_USD_ID: &str = "4001";

// ── FIX tag constants we actually touch ──────────────────────────────────
//
// MarketData group (snapshot / incremental refresh):
const TAG_MD_ENTRY_TYPE: u32 = 269;
const TAG_MD_ENTRY_PX: u32 = 270;
//
// NewOrderSingle (MsgType "D") / ExecutionReport (MsgType "8"):
const TAG_CL_ORD_ID: u32 = 11;
const TAG_EXEC_TYPE: u32 = 150;
const TAG_LAST_PX: u32 = 31;
const TAG_LAST_QTY: u32 = 32;
const TAG_TEXT: u32 = 58;

/// The traced payload that rides shared memory in both directions.
type Fill = Traced<RoundTrip, RoundTripLatency>;

/// Top-of-book in basis-points (price × 10 000) so we stay integer.
#[derive(Debug, Clone, Copy, Default)]
struct TopOfBook {
    bid_bps: i64,
    ask_bps: i64,
    last_update_ns: u64,
}

impl TopOfBook {
    fn is_ready(&self) -> bool {
        self.bid_bps > 0 && self.ask_bps > 0
    }
}

/// Event carried through the matcher's merged input stream.
///
/// `Default = None` lets the combine / fold pipeline use idle ticks (e.g.
/// non-ExecutionReport messages on the order session) without losing the
/// real ones — they fold to a no-op.
#[derive(Debug, Clone, Default)]
enum MatcherEvent {
    #[default]
    None,
    Order(Fill),
    Exec(FixMessage),
}

fn main() -> anyhow::Result<()> {
    env_logger::init();
    rustls::crypto::ring::default_provider()
        .install_default()
        .ok();

    let stamping = stamping();
    // LMAX London Demo updates EUR/USD only every few seconds during quiet
    // periods — observed gaps of 20+ seconds when the book is dormant — so
    // even 5 s rejects most orders outside of busy windows. 60 s keeps the
    // safety check meaningful while accepting the demo feed's real cadence.
    let max_md_age_ms = env_u64("WINGFOIL_MAX_MD_AGE_MS", 60_000);

    let username = required_env("LMAX_USERNAME")?;
    let password = required_env("LMAX_PASSWORD")?;

    log::info!(
        "fix_gw starting — stamping={stamping:?} max_md_age_ms={max_md_age_ms} as {username}"
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

    let book = build_top_of_book(&fix_md.data);
    let _md_sub = fix_md.fix_sub(g.constant(vec![EUR_USD_ID.to_string()]));
    let _md_status = log_status("md-session", &fix_md.status);
    let _ord_status = log_status("ord-session", &fix_ord.status);

    // ── Outbound: orders → price → stamp fix_send → (fork) ───────────────
    //
    // The whole pipeline stays **burst-shaped**. The obvious spelling here is
    // `.collapse::<Fill>()` straight off the subscriber, which is what this
    // example used to do — but `collapse` keeps only the burst's *last* value,
    // so every other order arriving in the same cycle was silently dropped and
    // never reached LMAX. `iceoryx2_sub` drains everything queued into one
    // burst, so multi-order bursts are not an edge case: they are what happens
    // whenever the publisher outruns a graph cycle, i.e. exactly under load.
    let orders = iceoryx2_sub::<Fill>(&g, RunMode::RealTime, SVC_ORDERS)?
        .inspect(|ts: &Burst<Fill>| {
            for t in ts.iter() {
                log::info!(
                    "fix_gw: order received via iceoryx2 cl_ord={} qty={} side={}",
                    cl_ord_id(&t.payload),
                    t.payload.qty,
                    t.payload.side,
                );
            }
        })
        .stamp_each_as::<round_trip_latency::gw_recv>(stamping);

    // `join_passive` is wingfoil's `bimap(Dep::Active(orders), Dep::Passive(book))`:
    // an order burst triggers the pricing, the book's current value is read but
    // does not trigger it. Every order in the burst is priced.
    let priced = orders
        .join_passive(&book, move |orders: &Burst<Fill>, book: &TopOfBook| {
            let now: u64 = NanoTime::now().into();
            let stale = !book.is_ready()
                || now.saturating_sub(book.last_update_ns) > max_md_age_ms * 1_000_000;
            let mut out = orders.clone();
            for order in out.iter_mut() {
                if stale {
                    log::warn!(
                        "skipping order seq={} — book stale or empty (last_update {} ns ago)",
                        order.payload.client_seq,
                        now.saturating_sub(book.last_update_ns),
                    );
                    continue;
                }
                order.payload.fill_price_bps = if order.payload.side == SIDE_BUY {
                    book.ask_bps
                } else {
                    book.bid_bps
                };
            }
            out
        })
        .stamp_each_all::<(round_trip_latency::gw_price, round_trip_latency::fix_send)>(stamping);

    // Side branch: send NewOrderSingle to the FIX order session via the
    // lock-free kanal channel.
    let sender = fix_ord.sender();
    let _inject_sink = priced.for_each(move |ts: &Burst<Fill>| {
        for t in ts.iter() {
            if t.payload.fill_price_bps == 0 {
                continue; // book was stale at pricing time — already logged
            }
            log::info!(
                "fix_gw: sending NewOrderSingle cl_ord={} px_bps={}",
                cl_ord_id(&t.payload),
                t.payload.fill_price_bps,
            );
            send_new_order_single(&sender, &t.payload);
        }
        Ok(())
    });

    // Main branch: into the matcher as Order events, one per order in the burst.
    let order_events: Stream<Burst<MatcherEvent>> = priced.map(|ts: &Burst<Fill>| {
        ts.iter()
            .map(|t| MatcherEvent::Order(*t))
            .collect::<Burst<MatcherEvent>>()
    });

    // Inbound: ExecutionReports from the order session. Filter here so only
    // MsgType=8 frames propagate — admin / heartbeat frames never show up in
    // the matcher. Filtering *within* the burst keeps every report: a single
    // TCP read can surface several, and `collapse` would have kept only the
    // last, leaving the others' orders parked forever.
    let exec_events: Stream<Burst<MatcherEvent>> = fix_ord.data.map(|ms: &Burst<FixMessage>| {
        ms.iter()
            .filter_map(|m| {
                if m.msg_type == "8" {
                    log::info!(
                        "fix_gw: ExecutionReport received cl_ord={} exec_type={}",
                        m.field(TAG_CL_ORD_ID).unwrap_or(""),
                        m.field(TAG_EXEC_TYPE).unwrap_or(""),
                    );
                    Some(MatcherEvent::Exec(m.clone()))
                } else {
                    log::debug!("fix_gw: non-exec msg_type={} dropped at filter", m.msg_type);
                    None
                }
            })
            .collect::<Burst<MatcherEvent>>()
    });

    // ── Matcher: combine + fold + map_filter ─────────────────────────────
    //
    // `combine` collects the ticked values from both upstreams into a single
    // `Burst` per cycle — if orders and ExecReports land in the same graph
    // cycle both show up, whereas `merge` would have kept only the first.
    // Because each upstream value is itself a `Burst<MatcherEvent>`, what
    // arrives is a burst *of* bursts; the fold walks both levels in order.
    //
    // `fold` carries the `RefCell<HashMap<ClOrdID, Fill>>` of parked orders in
    // its captured state — captured rather than folded, because `Fold`'s output
    // *is* its accumulator and is cloned every tick, which for a parked-order
    // map would mean copying the whole thing per cycle.
    //
    // The accumulator is the `Burst<Fill>` of everything matched **this cycle**,
    // cleared at the top of each fold. It used to be `Option<Fill>` — at most
    // one match per cycle — which was sound only because `collapse` upstream
    // had already thrown away any second event. Matching several orders in one
    // cycle is now ordinary, so there is no invariant to guard and the
    // "matcher invariant broken" error that used to live here is gone with it.

    let matched = {
        let park: RefCell<HashMap<String, Fill>> = RefCell::new(HashMap::new());
        g.combine(&[order_events, exec_events]).fold(
            Burst::<Fill>::default(),
            move |matched: &mut Burst<Fill>, groups: &Burst<Burst<MatcherEvent>>| {
                matched.clear();
                for ev in groups.iter().flat_map(|group| group.iter()) {
                    match ev {
                        MatcherEvent::Order(t) => {
                            let id = cl_ord_id(&t.payload);
                            park.borrow_mut().insert(id, *t);
                        }
                        MatcherEvent::Exec(msg) => {
                            let cl_ord = msg.field(TAG_CL_ORD_ID).unwrap_or("").to_string();
                            let exec_type = msg.field(TAG_EXEC_TYPE).unwrap_or("");
                            let Some(mut parked) = park.borrow_mut().remove(&cl_ord) else {
                                log::debug!(
                                    "unmatched exec report cl_ord_id={cl_ord} type={exec_type}"
                                );
                                continue;
                            };
                            match exec_type {
                                "F" | "1" | "2" => {
                                    // Fill (F) or partial-fill (1) or fully-filled (2).
                                    let last_qty: u64 = msg
                                        .field(TAG_LAST_QTY)
                                        .and_then(|s| s.parse().ok())
                                        .unwrap_or(0);
                                    let last_px: f64 = msg
                                        .field(TAG_LAST_PX)
                                        .and_then(|s| s.parse().ok())
                                        .unwrap_or(0.0);
                                    parked.payload.filled_qty = last_qty;
                                    parked.payload.fill_price_bps =
                                        (last_px * 10_000.0).round() as i64;
                                }
                                _ => {
                                    // Cancelled / Rejected — emit zero-fill so the
                                    // browser's round-trip counter still closes.
                                    let text = msg.field(TAG_TEXT).unwrap_or("");
                                    log::info!(
                                        "order cl_ord={cl_ord} terminal exec_type={exec_type} text={text}"
                                    );
                                    parked.payload.filled_qty = 0;
                                    parked.payload.fill_price_bps = 0;
                                }
                            }
                            matched.push(parked);
                        }
                        MatcherEvent::None => {}
                    }
                }
            },
        )
    };

    // Drop empty cycles, stamp the inbound stages, publish back via iceoryx2.
    let fills = matched
        .map_filter(|m: &Burst<Fill>| (m.clone(), !m.is_empty()))
        .stamp_each_all::<(round_trip_latency::fix_recv, round_trip_latency::gw_publish)>(stamping);

    let _pub_fills = fills
        .inspect(|ts: &Burst<Fill>| {
            for t in ts.iter() {
                log::info!(
                    "fix_gw: publishing fill cl_ord={} filled_qty={} px_bps={}",
                    cl_ord_id(&t.payload),
                    t.payload.filled_qty,
                    t.payload.fill_price_bps,
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

// ── ClOrdID and NewOrderSingle ───────────────────────────────────────────

/// `<sessionHex(last 8)>-<seq>` is unique by construction (session UUID is
/// random per browser tab, seq is monotonic per session).
/// LMAX ClOrdID has a 20-character limit, so use the last 8 hex chars only.
fn cl_ord_id(p: &RoundTrip) -> String {
    let full_hex = session_hex(&p.session);
    let short_hex = &full_hex[full_hex.len() - 8..];
    format!("{short_hex}-{}", p.client_seq)
}

/// Build and send a `NewOrderSingle` (MsgType `D`) to the LMAX order
/// session. Always IOC limit (TimeInForce=3, OrdType=2) at the price the
/// caller computed against the top-of-book — guarantees an immediate
/// terminal ExecutionReport (Fill, partial-fill-then-cancel, or reject)
/// so the round-trip closes cleanly.
///
/// [`FixSender::send`] is a non-blocking `try_send` on a bounded kanal
/// channel. On `QueueFull` we log loudly and drop — for the demo that's
/// the least-bad option; a production gateway would propagate the
/// rejection back to the browser.
fn send_new_order_single(sender: &FixSender, p: &RoundTrip) {
    let cl_ord = cl_ord_id(p);
    let side = if p.side == SIDE_BUY { "1" } else { "2" };
    let price = (p.fill_price_bps as f64) / 10_000.0;
    let transact_time = chrono::Utc::now().format("%Y%m%d-%H:%M:%S%.3f").to_string();
    let fields = vec![
        (TAG_CL_ORD_ID, cl_ord.clone()),
        (22, "8".into()),            // SecurityIDSource = ExchangeSymbol
        (48, EUR_USD_ID.into()),     // SecurityID
        (54, side.into()),           // Side
        (38, p.qty.to_string()),     // OrderQty
        (40, "2".into()),            // OrdType = Limit
        (44, format!("{price:.5}")), // Price
        (59, "3".into()),            // TimeInForce = IOC
        (60, transact_time),         // TransactTime
    ];
    if let Err(e) = sender.send(FixMessage {
        msg_type: "D".into(),
        seq_num: 0,
        sending_time: NanoTime::ZERO,
        fields,
    }) {
        log::error!("FixSender dropped NewOrderSingle cl_ord={cl_ord}: {e}");
    }
}

// ── Top-of-book builder from the LMAX MD stream ──────────────────────────
//
// LMAX sends MarketDataSnapshotFullRefresh (MsgType W) and
// MarketDataIncrementalRefresh (X) with repeating groups of
// (269 MDEntryType, 270 MDEntryPx, 271 MDEntrySize). We don't need the
// size for this demo so we walk the fields linearly: when we see a 269
// the next matching 270 in the same group sets bid (269=0) or ask (269=1).

// This used to `collapse()` the burst before folding, which meant that when a
// single cycle carried several refreshes — a bid update followed by an ask
// update, say — only the last survived and the earlier side never reached the
// book. Fold the whole burst instead.
//
// It was also, briefly, a `nitro!` compiled island: `collapse` + `fold` behind
// one node. Removing the `collapse` leaves a single `fold`, and an island
// wrapping one node is strictly worse than the node (same dyn call, plus the
// composite's boundary), so the island went with it. See the README's
// "Compiled islands" section for why nothing else here can be one either.
fn build_top_of_book(data: &Stream<Burst<FixMessage>>) -> Stream<TopOfBook> {
    data.fold(
        TopOfBook::default(),
        |book: &mut TopOfBook, msgs: &Burst<FixMessage>| {
            let now: u64 = NanoTime::now().into();
            for msg in msgs.iter() {
                if !matches!(msg.msg_type.as_str(), "W" | "X") {
                    continue;
                }
                let mut current_type: Option<u8> = None;
                for (tag, val) in &msg.fields {
                    match *tag {
                        TAG_MD_ENTRY_TYPE => {
                            current_type = val.parse::<u8>().ok();
                        }
                        TAG_MD_ENTRY_PX => {
                            if let (Some(t), Ok(px)) = (current_type, val.parse::<f64>()) {
                                let bps = (px * 10_000.0).round() as i64;
                                match t {
                                    0 => book.bid_bps = bps, // Bid
                                    1 => book.ask_bps = bps, // Offer
                                    _ => {}
                                }
                                book.last_update_ns = now;
                            }
                        }
                        _ => {}
                    }
                }
            }
        },
    )
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
