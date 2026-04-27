// End-to-end latency demo — FIX gateway.
//
// Two FIX sessions, both TLS to the LMAX London Demo:
//   * MD session    — fix-marketdata.london-demo.lmax.com:443  (LMXBDM)
//                     subscribes to EUR/USD; folded into a top-of-book.
//   * Order session — fix-order.london-demo.lmax.com:443       (LMXBD)
//                     receives NewOrderSingle injections and surfaces
//                     ExecutionReports.
//
// Pipeline (this binary):
//   ws_server ── iceoryx2 ──► fix_gw ──── FIX/TLS ──► LMAX                (orders)
//   ws_server ◄── iceoryx2 ── fix_gw ◄─── FIX/TLS ─── LMAX                (fills)
//
// Stamps `gw_recv` and `gw_price` on the way out, `fix_send` just before
// FIX injection, then on the inbound side `fix_recv` (when the
// ExecutionReport surfaces) and `gw_publish` (just before iceoryx2
// publish). The four stages on the WS edge live in ws_server.
//
// Run:
//   LMAX_USERNAME=xxx LMAX_PASSWORD=yyy \
//     cargo run --release --example latency_e2e_fix_gw \
//     --features "fix,iceoryx2-beta" -- [--precise]
//
// Without LMAX_USERNAME / LMAX_PASSWORD the binary refuses to start —
// real order routing requires real creds. (We deliberately removed the
// "simulated fill" fallback so the latency report only ever shows
// honest end-to-end numbers.)

#[path = "shared.rs"]
mod shared;

use std::cell::RefCell;
use std::collections::HashMap;
use std::rc::Rc;

use wingfoil::adapters::fix::{FixMessage, FixSender, FixSessionStatus, fix_connect_tls};
use wingfoil::adapters::iceoryx2::{iceoryx2_pub, iceoryx2_sub};
use wingfoil::*;

use shared::{
    RoundTrip, RoundTripLatency, SIDE_BUY, SVC_FILLS, SVC_ORDERS, env_u64, pin_current_from_env,
    precise_stamps_enabled, round_trip_latency, session_hex,
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
/// `Default = None` lets the merge / fold pipeline use idle ticks (e.g.
/// non-ExecutionReport messages on the order session) without losing the
/// real ones — they fold to a no-op.
#[derive(Debug, Clone, Default)]
enum MatcherEvent {
    #[default]
    None,
    Order(Traced<RoundTrip, RoundTripLatency>),
    Exec(FixMessage),
}

fn main() -> anyhow::Result<()> {
    env_logger::init();
    rustls::crypto::ring::default_provider()
        .install_default()
        .ok();

    let precise = precise_stamps_enabled();
    // LMAX London Demo updates EUR/USD only every few seconds during quiet
    // periods — observed gaps of 20+ seconds when the book is dormant — so
    // even 5 s rejects most orders outside of busy windows. 60 s keeps the
    // safety check meaningful while accepting the demo feed's real cadence.
    let max_md_age_ms = env_u64("WINGFOIL_MAX_MD_AGE_MS", 60_000);

    let username = std::env::var("LMAX_USERNAME")
        .map_err(|_| anyhow::anyhow!("LMAX_USERNAME env var is required"))?;
    let password = std::env::var("LMAX_PASSWORD")
        .map_err(|_| anyhow::anyhow!("LMAX_PASSWORD env var is required"))?;

    log::info!("fix_gw starting — precise={precise} max_md_age_ms={max_md_age_ms} as {username}");

    // ── Two FIX sessions ─────────────────────────────────────────────────
    log::info!("connecting MD    session {LMAX_HOST_MD}:{LMAX_PORT} target={LMAX_TARGET_MD}");
    let fix_md = fix_connect_tls(
        LMAX_HOST_MD,
        LMAX_PORT,
        &username,
        LMAX_TARGET_MD,
        Some(&password),
    );
    log::info!("connecting Order session {LMAX_HOST_ORD}:{LMAX_PORT} target={LMAX_TARGET_ORD}");
    let fix_ord = fix_connect_tls(
        LMAX_HOST_ORD,
        LMAX_PORT,
        &username,
        LMAX_TARGET_ORD,
        Some(&password),
    );

    let book = build_top_of_book(fix_md.data.clone());
    let md_sub = fix_md.fix_sub(constant(vec![EUR_USD_ID.into()]));
    let md_status = log_status("md-session", fix_md.status.clone());
    let ord_status = log_status("ord-session", fix_ord.status.clone());

    // ── Outbound: orders → price → stamp fix_send → (fork) ───────────────
    let orders = iceoryx2_sub::<Traced<RoundTrip, RoundTripLatency>>(SVC_ORDERS)
        .collapse::<Traced<RoundTrip, RoundTripLatency>>()
        .stamp_if::<round_trip_latency::gw_recv>(!precise)
        .stamp_precise_if::<round_trip_latency::gw_recv>(precise);

    let priced = bimap(
        Dep::Active(orders),
        Dep::Passive(book),
        move |mut order: Traced<RoundTrip, RoundTripLatency>, book: TopOfBook| {
            let now: u64 = NanoTime::now().into();
            if !book.is_ready()
                || now.saturating_sub(book.last_update_ns) > max_md_age_ms * 1_000_000
            {
                log::warn!(
                    "skipping order seq={} — book stale or empty (last_update {} ns ago)",
                    order.payload.client_seq,
                    now.saturating_sub(book.last_update_ns),
                );
                return order;
            }
            order.payload.fill_price_bps = if order.payload.side == SIDE_BUY {
                book.ask_bps
            } else {
                book.bid_bps
            };
            order
        },
    )
    .stamp_if::<round_trip_latency::gw_price>(!precise)
    .stamp_precise_if::<round_trip_latency::gw_price>(precise)
    .stamp_if::<round_trip_latency::fix_send>(!precise)
    .stamp_precise_if::<round_trip_latency::fix_send>(precise);

    // Side branch: send NewOrderSingle to the FIX order session via the
    // lock-free kanal channel (see wingfoil FIX adapter PR #225).
    let sender = fix_ord.sender();
    let inject_sink = priced.clone().for_each(move |t, _time| {
        if t.payload.fill_price_bps == 0 {
            return; // book was stale at pricing time — already logged
        }
        send_new_order_single(&sender, &t.payload);
    });

    // Main branch: into the matcher as Order events.
    let order_events: Rc<dyn Stream<MatcherEvent>> = priced.map(MatcherEvent::Order);

    // Inbound: ExecutionReports from the order session. Filter here so
    // only MsgType=8 frames propagate — admin / heartbeat frames never
    // show up in the matcher's burst.
    let exec_events: Rc<dyn Stream<MatcherEvent>> = MapFilterStream::new(
        fix_ord.data.clone().collapse::<FixMessage>(),
        Box::new(|m: FixMessage| {
            let is_exec = m.msg_type == "8";
            (MatcherEvent::Exec(m), is_exec)
        }),
    )
    .into_stream();

    // ── Matcher: combine + fold + map_filter ─────────────────────────────
    //
    // `combine` collects the ticked values from both upstreams into a
    // single `Burst<MatcherEvent>` per cycle — if an Order and an
    // ExecReport land in the same graph cycle both show up, whereas
    // `merge` would have kept only the first. `fold` carries the
    // `RefCell<HashMap<ClOrdID, Traced>>` of parked orders in its
    // captured state. Each cycle, we walk the burst in order, parking
    // orders or matching ExecReports. The output value is the last
    // matched fill this cycle (or None); downstream `MapFilterStream`
    // drops the Nones.

    let matched = {
        let park: RefCell<HashMap<String, Traced<RoundTrip, RoundTripLatency>>> =
            RefCell::new(HashMap::new());
        combine(vec![order_events, exec_events]).fold(
            move |last: &mut Option<Traced<RoundTrip, RoundTripLatency>>,
                  burst: Burst<MatcherEvent>| {
                *last = None;
                for ev in burst {
                    match ev {
                        MatcherEvent::Order(t) => {
                            let id = cl_ord_id(&t.payload);
                            park.borrow_mut().insert(id, t);
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
                            *last = Some(parked);
                        }
                        MatcherEvent::None => {}
                    }
                }
            },
        )
    };

    // Drop Nones, stamp the inbound stages, publish back via iceoryx2.
    let fills = MapFilterStream::new(
        matched,
        Box::new(|v: Option<Traced<RoundTrip, RoundTripLatency>>| {
            let ticked = v.is_some();
            (v.unwrap_or_default(), ticked)
        }),
    )
    .into_stream()
    .stamp_if::<round_trip_latency::fix_recv>(!precise)
    .stamp_precise_if::<round_trip_latency::fix_recv>(precise)
    .stamp_if::<round_trip_latency::gw_publish>(!precise)
    .stamp_precise_if::<round_trip_latency::gw_publish>(precise);

    let pub_fills = iceoryx2_pub(fills.map(|t| burst![t]), SVC_FILLS);

    let nodes: Vec<Rc<dyn Node>> = vec![
        pub_fills,
        inject_sink,
        md_sub,
        md_status,
        ord_status,
        fix_md.data.as_node(),
        fix_ord.data.as_node(),
    ];

    // Pin AFTER all adapter workers (FIX sessions, iceoryx2 pub/sub) are
    // spawned so they keep the default affinity mask. The pinned mask
    // applies only to the graph cycle thread (this one).
    pin_current_from_env("WINGFOIL_PIN_GRAPH");

    Graph::new(nodes, RunMode::RealTime, RunFor::Forever).run()?;
    Ok(())
}

// ── ClOrdID and NewOrderSingle ───────────────────────────────────────────

/// `<sessionHex(last 8)>-<seq>` is unique by construction (session UUID is
/// random per browser tab, seq is monotonic per session).
/// LMAX ClOrdID has 20-character limit, so use last 8 hex chars only.
fn cl_ord_id(p: &RoundTrip) -> String {
    let full_hex = session_hex(&p.session);
    let short_hex = &full_hex[full_hex.len() - 8..];
    format!("{}-{}", short_hex, p.client_seq)
}

/// Build and send a `NewOrderSingle` (MsgType `D`) to the LMAX order
/// session. Always IOC limit (TimeInForce=3, OrdType=2) at the price the
/// caller computed against the top-of-book — guarantees an immediate
/// terminal ExecutionReport (Fill, partial-fill-then-cancel, or reject)
/// so the round-trip closes cleanly.
///
/// `FixSender::send` is a non-blocking `try_send` on a bounded kanal
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

fn build_top_of_book(data: Rc<dyn Stream<Burst<FixMessage>>>) -> Rc<dyn Stream<TopOfBook>> {
    data.collapse::<FixMessage>()
        .fold(|book: &mut TopOfBook, msg: FixMessage| {
            if !matches!(msg.msg_type.as_str(), "W" | "X") {
                return;
            }
            let now: u64 = NanoTime::now().into();
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
        })
}

fn log_status(
    label: &'static str,
    status: Rc<dyn Stream<Burst<FixSessionStatus>>>,
) -> Rc<dyn Node> {
    let cell: RefCell<FixSessionStatus> = RefCell::new(FixSessionStatus::Disconnected);
    status.for_each(move |burst, _t| {
        for st in burst {
            if st != *cell.borrow() {
                log::info!("{label}: {st:?}");
                *cell.borrow_mut() = st;
            }
        }
    })
}
