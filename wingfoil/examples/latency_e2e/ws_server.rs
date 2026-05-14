// End-to-end latency demo — WebSocket server edge.
//
// Pipeline (this binary):
//   browser ── WS ──► ws_server                                    (control: start/stop)
//                        │
//                        └── server-side generator ── iceoryx2 ──► fix_gw   (outbound)
//   browser ◄── WS ── ws_server ◄── iceoryx2 ── fix_gw            (inbound)
//
// The browser no longer publishes individual orders. It sends one
// `ControlFrame { action: START, side, qty, rate_hz }` to enable a
// server-side generator bound to its session UUID, then a matching
// `STOP` (or session TTL) to disable it. Re-sending `START` while
// already active updates side/qty/rate_hz live — no stop/restart cycle.
//
// Stamps `ws_recv` (when the generator produces the order) and
// `ws_publish` (just before the iceoryx2 publish) on the way out;
// `ws_sub_recv` and `ws_send` on the way back. The cap + auto-expiry
// protect the single LMAX session living in fix_gw.
//
// Run (after starting fix_gw):
//   cargo run --example latency_e2e_ws_server \
//     --features "web-tls,iceoryx2,prometheus,otlp" -- \
//     --addr 0.0.0.0:8080 [--no-precise] \
//     [--tls-cert /etc/wingfoil/tls/cert.pem --tls-key /etc/wingfoil/tls/key.pem]
//
// Passing --tls-cert/--tls-key (or setting WINGFOIL_TLS_CERT/WINGFOIL_TLS_KEY)
// switches the server to HTTPS + WSS on the same port. The browser
// client (static/app.js) auto-detects scheme from `location.protocol`,
// so no client-side config flips are needed.

#[path = "shared.rs"]
mod shared;

use std::collections::HashMap;
use std::path::PathBuf;
use std::rc::Rc;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use iceoryx2::prelude::ZeroCopySend;
use wingfoil::adapters::iceoryx2::{iceoryx2_pub, iceoryx2_sub};
use wingfoil::adapters::otlp::{OtlpAttributeBuffer, OtlpConfig, OtlpSpans};
use wingfoil::adapters::prometheus::PrometheusExporter;
use wingfoil::adapters::web::{CodecKind, WebPubOperators, WebServer, web_sub};
use wingfoil::*;

use shared::{
    CONTROL_START, CONTROL_STOP, ControlFrame, FillFrame, RoundTrip, RoundTripLatency, SIDE_ALT,
    SVC_FILLS, SVC_ORDERS, SessionId, TOPIC_CONTROL, TOPIC_FILLS, env_string, env_u64,
    pin_current_from_env, precise_stamps_enabled, round_trip_latency, session_hex,
};

// ── Session registry ──────────────────────────────────────────────────────
//
// Lives in an `Arc<Mutex<…>>` because the Prometheus gauges read it from
// the graph thread, the control web_sub writes to it from its async
// consumer thread, and the generator ticker reads it from the graph
// thread. Every operation is O(active_sessions ≤ cap = 8) and is well
// off the FIX-RTT critical path; contention is negligible.

#[derive(Debug)]
struct ActiveSession {
    side: u8,     // SIDE_BUY, SIDE_SELL, or SIDE_ALT
    alt_next: u8, // next concrete side when `side == SIDE_ALT`
    qty: u64,
    period_ns: u64,    // 1e9 / rate_hz; 0 disables emission
    next_emit_ns: u64, // next due time on the wall clock
    client_seq: u64,
    admitted_at_ns: u64, // for TTL-based eviction
}

#[derive(Default)]
struct Sessions {
    active: HashMap<SessionId, ActiveSession>,
    cap: usize,
    ttl_ns: u64,
    admitted_total: u64,
    rejected_total: u64,
    emitted_total: u64,
}

impl Sessions {
    fn new(cap: usize, ttl_secs: u64) -> Self {
        Self {
            cap,
            ttl_ns: ttl_secs * 1_000_000_000,
            ..Default::default()
        }
    }

    fn gc(&mut self, now_ns: u64) {
        self.active
            .retain(|_, e| now_ns.saturating_sub(e.admitted_at_ns) < self.ttl_ns);
    }

    /// Apply a control message. `START` admits a new session or updates an
    /// existing one in-place (so the browser can change `rate_hz` without
    /// stop/restart). `STOP` removes it.
    fn apply(&mut self, frame: &ControlFrame, now_ns: u64) {
        self.gc(now_ns);
        match frame.action {
            CONTROL_STOP => {
                self.active.remove(&frame.session);
            }
            CONTROL_START => {
                let period_ns = if frame.rate_hz == 0 {
                    0
                } else {
                    1_000_000_000 / frame.rate_hz as u64
                };
                if let Some(s) = self.active.get_mut(&frame.session) {
                    s.side = frame.side;
                    s.qty = frame.qty;
                    s.period_ns = period_ns;
                    // Re-anchor the schedule. Without this, raising rate_hz
                    // from 1 to 100 would still wait ~1 s for the next emit;
                    // lowering rate_hz to 1 mid-burst would let it catch up
                    // by firing many back-to-back orders.
                    s.next_emit_ns = now_ns.saturating_add(period_ns);
                    return;
                }
                if self.active.len() >= self.cap {
                    self.rejected_total += 1;
                    log::warn!(
                        "rejected start session={} (cap {} reached)",
                        session_hex(&frame.session),
                        self.cap,
                    );
                    return;
                }
                self.active.insert(
                    frame.session,
                    ActiveSession {
                        side: frame.side,
                        alt_next: 0,
                        qty: frame.qty,
                        period_ns,
                        next_emit_ns: now_ns,
                        client_seq: 0,
                        admitted_at_ns: now_ns,
                    },
                );
                self.admitted_total += 1;
                log::info!(
                    "admitted session={} rate={}Hz qty={} side={}",
                    session_hex(&frame.session),
                    frame.rate_hz,
                    frame.qty,
                    frame.side,
                );
            }
            other => log::warn!(
                "unknown control action={other} session={}",
                session_hex(&frame.session),
            ),
        }
    }

    /// Emit at most one order this cycle. Picks the session whose
    /// `next_emit_ns` is smallest among those past due; ties broken by
    /// HashMap iteration order. Advances that session's schedule and
    /// returns the order — None if no session is due.
    fn poll(&mut self, now_ns: u64) -> Option<RoundTrip> {
        self.gc(now_ns);
        let mut chosen: Option<(SessionId, u64)> = None;
        for (id, s) in self.active.iter() {
            if s.period_ns == 0 || s.next_emit_ns > now_ns {
                continue;
            }
            if chosen.is_none_or(|(_, t)| s.next_emit_ns < t) {
                chosen = Some((*id, s.next_emit_ns));
            }
        }
        let (id, _) = chosen?;
        let s = self.active.get_mut(&id).expect("just chosen above");
        let side = if s.side == SIDE_ALT {
            let v = s.alt_next;
            s.alt_next ^= 1;
            v
        } else {
            s.side
        };
        s.client_seq += 1;
        s.next_emit_ns = s.next_emit_ns.saturating_add(s.period_ns);
        // Cap catch-up: don't backlog if the graph stalled or rate_hz spiked.
        if s.next_emit_ns < now_ns {
            s.next_emit_ns = now_ns.saturating_add(s.period_ns);
        }
        self.emitted_total += 1;
        Some(RoundTrip {
            session: id,
            client_seq: s.client_seq,
            qty: s.qty,
            side,
            ..Default::default()
        })
    }
}

// ── Main ──────────────────────────────────────────────────────────────────

fn main() -> anyhow::Result<()> {
    env_logger::init();
    let args: Vec<String> = std::env::args().collect();
    let addr = args
        .iter()
        .position(|a| a == "--addr")
        .and_then(|i| args.get(i + 1).cloned())
        .unwrap_or_else(|| env_string("WINGFOIL_WEB_ADDR", "0.0.0.0:8080"));
    let metrics_addr = env_string("WINGFOIL_METRICS_ADDR", "0.0.0.0:9091");
    let session_cap = env_u64("WINGFOIL_SESSION_CAP", 8) as usize;
    let session_ttl = env_u64("WINGFOIL_SESSION_SECS", 60);
    let tick_us = env_u64("WINGFOIL_TICK_US", 1_000);
    let precise = precise_stamps_enabled();

    let static_dir: PathBuf = std::env::var("WINGFOIL_STATIC_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|_| {
            PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("examples/latency_e2e/static")
        });

    // TLS material is opt-in: missing cert/key keeps the server on plain
    // HTTP/WS, matching the existing local-dev workflow. Pulumi user_data
    // wires both args so the deployed boxes serve HTTPS by default.
    let arg_or_env = |flag: &str, var: &str| -> Option<PathBuf> {
        args.iter()
            .position(|a| a == flag)
            .and_then(|i| args.get(i + 1).cloned())
            .or_else(|| std::env::var(var).ok())
            .map(PathBuf::from)
    };
    let tls_cert = arg_or_env("--tls-cert", "WINGFOIL_TLS_CERT");
    let tls_key = arg_or_env("--tls-key", "WINGFOIL_TLS_KEY");
    if tls_cert.is_some() != tls_key.is_some() {
        anyhow::bail!("--tls-cert and --tls-key must be set together");
    }

    let mut builder = WebServer::bind(&addr)
        .codec(CodecKind::Json)
        .serve_static(&static_dir);
    if let (Some(cert), Some(key)) = (tls_cert.as_ref(), tls_key.as_ref()) {
        builder = builder.tls(cert, key);
    }
    let server = builder.start()?;
    let scheme = if server.is_tls() { "https" } else { "http" };
    log::info!(
        "ws_server listening on {scheme}://{} (port {}) — static dir {}",
        addr,
        server.port(),
        static_dir.display(),
    );
    log::info!("session_cap={session_cap} ttl={session_ttl}s tick={tick_us}us precise={precise}",);

    let sessions = Arc::new(Mutex::new(Sessions::new(session_cap, session_ttl)));

    // ── Control ingest ───────────────────────────────────────────────────
    // Drives the live state. Browser publishes one `ControlFrame` per
    // start/stop click (and again to update side/qty/rate without
    // stopping); the for_each just mutates the shared session map.
    let control_in = web_sub::<ControlFrame>(&server, TOPIC_CONTROL).collapse::<ControlFrame>();
    let control_sink = {
        let s = sessions.clone();
        control_in.for_each(move |frame, _t| {
            let now_ns: u64 = NanoTime::now().into();
            s.lock().unwrap().apply(&frame, now_ns);
        })
    };

    // ── Outbound leg (server-side generator) ─────────────────────────────
    // Single fast ticker; on each cycle, ask the registry for the next
    // due order (or None). MapFilterStream drops the Nones, then we wrap
    // in Traced<…>, stamp, and publish to iceoryx2 exactly as before.
    let gen_tick = ticker(Duration::from_micros(tick_us));
    let raw_orders = {
        let s = sessions.clone();
        gen_tick.produce(move || s.lock().unwrap().poll(NanoTime::now().into()))
    };
    let due_orders = MapFilterStream::new(
        raw_orders,
        Box::new(|opt: Option<RoundTrip>| match opt {
            Some(rt) => (rt, true),
            None => (RoundTrip::default(), false),
        }),
    )
    .into_stream();

    let traced_orders = due_orders
        .map(Traced::<RoundTrip, RoundTripLatency>::new)
        .stamp_if::<round_trip_latency::ws_recv>(!precise)
        .stamp_precise_if::<round_trip_latency::ws_recv>(precise)
        .stamp_if::<round_trip_latency::ws_publish>(!precise)
        .stamp_precise_if::<round_trip_latency::ws_publish>(precise);

    let pub_orders = iceoryx2_pub(traced_orders.map(|t| burst![t]), SVC_ORDERS);

    // ── Inbound leg ──────────────────────────────────────────────────────
    let fills_in = iceoryx2_sub::<Traced<RoundTrip, RoundTripLatency>>(SVC_FILLS)
        .collapse::<Traced<RoundTrip, RoundTripLatency>>()
        .stamp_if::<round_trip_latency::ws_sub_recv>(!precise)
        .stamp_precise_if::<round_trip_latency::ws_sub_recv>(precise)
        .stamp_if::<round_trip_latency::ws_send>(!precise)
        .stamp_precise_if::<round_trip_latency::ws_send>(precise);

    let (inbound_report, inbound_stats) = fills_in.latency_report(true);

    // OTLP trace export — one parent span + one child per hop, per fill.
    // The attribute extractor pushes session.id and client_seq onto the
    // parent span so the Grafana dashboard can filter by session (which
    // Prometheus can't do without blowing up cardinality).
    let otlp_endpoint = env_string("WINGFOIL_OTLP_ENDPOINT", "http://localhost:4318");
    log::info!("otlp trace export → {otlp_endpoint}");
    let span_sink = fills_in.clone().otlp_spans(
        OtlpConfig {
            endpoint: otlp_endpoint,
            service_name: "wingfoil-latency-e2e".into(),
        },
        "roundtrip",
        |t: &Traced<RoundTrip, RoundTripLatency>, attrs: &mut OtlpAttributeBuffer| {
            attrs.add("session.id", session_hex(&t.payload.session));
            attrs.add("client_seq", t.payload.client_seq as i64);
            attrs.add("side", if t.payload.side == 0 { "buy" } else { "sell" });
            attrs.add("filled_qty", t.payload.filled_qty as i64);
        },
    );

    let fill_frames = fills_in.map(|t: Traced<RoundTrip, RoundTripLatency>| {
        let l = t.latency;
        FillFrame {
            session: t.payload.session,
            client_seq: t.payload.client_seq,
            side: t.payload.side,
            filled_qty: t.payload.filled_qty,
            fill_price_bps: t.payload.fill_price_bps,
            stamps: [
                l.ws_recv,
                l.ws_publish,
                l.gw_recv,
                l.gw_price,
                l.fix_send,
                l.fix_recv,
                l.gw_publish,
                l.ws_sub_recv,
                l.ws_send,
            ],
        }
    });
    let pub_fills = fill_frames.web_pub(&server, TOPIC_FILLS);

    // ── Prometheus metrics ───────────────────────────────────────────────
    let exporter = PrometheusExporter::new(&metrics_addr);
    let metrics_port = exporter.serve()?;
    log::info!("prometheus on http://{metrics_addr}/metrics (bound :{metrics_port})");

    let tick_1s = ticker(Duration::from_secs(1));
    let active_gauge = {
        let s = sessions.clone();
        tick_1s.produce(move || s.lock().unwrap().active.len() as u64)
    };
    let admitted_gauge = {
        let s = sessions.clone();
        tick_1s.produce(move || s.lock().unwrap().admitted_total)
    };
    let rejected_gauge = {
        let s = sessions.clone();
        tick_1s.produce(move || s.lock().unwrap().rejected_total)
    };
    let emitted_gauge = {
        let s = sessions.clone();
        tick_1s.produce(move || s.lock().unwrap().emitted_total)
    };

    let mut nodes: Vec<Rc<dyn Node>> = vec![
        control_sink,
        pub_orders,
        pub_fills,
        inbound_report,
        span_sink,
        exporter.register("latency_e2e_active_sessions", active_gauge),
        exporter.register("latency_e2e_admitted_total", admitted_gauge),
        exporter.register("latency_e2e_rejected_total", rejected_gauge),
        exporter.register("latency_e2e_emitted_total", emitted_gauge),
    ];
    nodes.extend(register_stage_metrics(&exporter, &inbound_stats));

    // Pin AFTER all adapter workers (web server, iceoryx2 pub/sub,
    // Prometheus exporter, OTLP exporter) are spawned so they keep the
    // default affinity mask. The pinned mask applies only to the graph
    // cycle thread (this one).
    pin_current_from_env("WINGFOIL_PIN_GRAPH");

    Graph::new(nodes, RunMode::RealTime, RunFor::Forever).run()?;
    Ok(())
}

// ── Per-stage Prometheus gauges ──────────────────────────────────────────
//
// The Prometheus adapter has no native label support, so we register one
// gauge per (stage × statistic) tuple. The Grafana dashboard groups them
// back together via metric-name regex.

fn register_stage_metrics<L: Latency>(
    exporter: &PrometheusExporter,
    stats: &Rc<std::cell::RefCell<LatencyStats<L>>>,
) -> Vec<Rc<dyn Node>> {
    let mut out: Vec<Rc<dyn Node>> = Vec::new();
    let names = L::stage_names();
    for i in 1..L::N {
        let stage = format!("{}__{}", names[i - 1], names[i]);
        let tick = ticker(Duration::from_secs(1));
        let p50 = {
            let st = stats.clone();
            tick.produce(move || st.borrow().stages[i].quantile_ns(0.5))
        };
        let p99 = {
            let st = stats.clone();
            tick.produce(move || st.borrow().stages[i].quantile_ns(0.99))
        };
        let count = {
            let st = stats.clone();
            tick.produce(move || st.borrow().stages[i].count)
        };
        out.push(exporter.register(format!("latency_e2e_{stage}_p50_ns"), p50));
        out.push(exporter.register(format!("latency_e2e_{stage}_p99_ns"), p99));
        out.push(exporter.register(format!("latency_e2e_{stage}_count_total"), count));
    }
    out
}

// Compile-time sanity: the iceoryx2 payload type is zero-copy sendable.
const _: fn() = || {
    fn assert_zc<T: ZeroCopySend>() {}
    assert_zc::<Traced<RoundTrip, RoundTripLatency>>();
};
