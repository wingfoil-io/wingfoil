// End-to-end latency demo — WebSocket server edge.
//
// Pipeline (this binary):
//   browser ── WS ──► ws_server ── iceoryx2 ──► fix_gw            (outbound)
//   browser ◄── WS ── ws_server ◄── iceoryx2 ── fix_gw            (inbound)
//   browser ── WS ──► ws_server                                    (echo)
//
// Stamps `ws_recv` and `ws_publish` on the way out, `ws_sub_recv` and
// `ws_send` on the way back. Enforces a global cap on concurrent sessions
// and auto-expires each session after 60 s — protects the single LMAX
// session living in fix_gw and keeps a public deployment bounded.
//
// Run (after starting fix_gw):
//   cargo run --example latency_e2e_ws_server \
//     --features "web,iceoryx2-beta,prometheus" -- \
//     --addr 0.0.0.0:8080 [--precise]

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
    EchoFrame, FillFrame, OrderFrame, RoundTrip, RoundTripLatency, SVC_FILLS, SVC_ORDERS,
    SessionId, TOPIC_ECHO, TOPIC_FILLS, TOPIC_ORDERS, env_string, env_u64, precise_stamps_enabled,
    round_trip_latency, session_hex,
};

// ── Session registry ──────────────────────────────────────────────────────
//
// Lives in an `Arc<Mutex<…>>` because the Prometheus gauges read it from
// the graph thread while the order pipeline writes to it from the web_sub
// async consumer thread. Every order frame touches this once; contention
// is negligible.

#[derive(Debug, Clone, Copy)]
struct SessionEntry {
    admitted_at_ns: u64,
    orders: u64,
}

#[derive(Default)]
struct Sessions {
    active: HashMap<SessionId, SessionEntry>,
    cap: usize,
    ttl_ns: u64,
    admitted_total: u64,
    rejected_total: u64,
}

impl Sessions {
    fn new(cap: usize, ttl_secs: u64) -> Self {
        Self {
            cap,
            ttl_ns: ttl_secs * 1_000_000_000,
            ..Default::default()
        }
    }

    /// Returns `true` if the order should be forwarded.
    fn admit(&mut self, id: &SessionId, now_ns: u64) -> bool {
        self.active
            .retain(|_, e| now_ns.saturating_sub(e.admitted_at_ns) < self.ttl_ns);
        if let Some(e) = self.active.get_mut(id) {
            e.orders += 1;
            return true;
        }
        if self.active.len() >= self.cap {
            self.rejected_total += 1;
            return false;
        }
        self.active.insert(
            *id,
            SessionEntry {
                admitted_at_ns: now_ns,
                orders: 1,
            },
        );
        self.admitted_total += 1;
        true
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
    let precise = precise_stamps_enabled();

    let static_dir: PathBuf = [
        env!("CARGO_MANIFEST_DIR"),
        "examples",
        "latency_e2e",
        "static",
    ]
    .iter()
    .collect();

    let server = WebServer::bind(&addr)
        .codec(CodecKind::Json)
        .serve_static(&static_dir)
        .start()?;
    log::info!(
        "ws_server listening on http://{} (port {}) — static dir {}",
        addr,
        server.port(),
        static_dir.display(),
    );
    log::info!("session_cap={session_cap} ttl={session_ttl}s precise={precise}");

    let sessions = Arc::new(Mutex::new(Sessions::new(session_cap, session_ttl)));

    // ── Outbound leg ─────────────────────────────────────────────────────
    let orders_in = web_sub::<OrderFrame>(&server, TOPIC_ORDERS).collapse::<OrderFrame>();
    let admitted = {
        let s = sessions.clone();
        MapFilterStream::new(
            orders_in,
            Box::new(move |frame: OrderFrame| {
                let now_ns: u64 = NanoTime::now().into();
                let admit = s.lock().unwrap().admit(&frame.session, now_ns);
                if !admit {
                    log::warn!(
                        "rejected order session={} seq={} (cap reached)",
                        session_hex(&frame.session),
                        frame.client_seq,
                    );
                }
                (frame, admit)
            }),
        )
        .into_stream()
    };

    let traced_orders = admitted
        .map(|o: OrderFrame| {
            Traced::<RoundTrip, RoundTripLatency>::new(RoundTrip {
                session: o.session,
                client_seq: o.client_seq,
                qty: o.qty,
                side: o.side,
                t_client_send: o.t_client_send,
                ..Default::default()
            })
        })
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
            t_client_send: t.payload.t_client_send,
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

    // ── Echo leg (round-trip from browser) ───────────────────────────────
    //
    // The echo carries four stamps: T1 (client send), T2/T3 (server
    // ws_recv / ws_send), T4 (client recv). Every delta we care about
    // lives inside a single clock domain:
    //     rtt_total         = T4 - T1            (client clock)
    //     server_resident   = T3 - T2            (server clock)
    //     wire_rtt          = rtt_total - server_resident
    // All three are subtractions within one clock — no NTP-style offset
    // estimation needed.

    let echoes = web_sub::<EchoFrame>(&server, TOPIC_ECHO).collapse::<EchoFrame>();

    let rtt_stats: Rc<std::cell::RefCell<StageStats>> =
        Rc::new(std::cell::RefCell::new(StageStats::default()));
    let wire_stats: Rc<std::cell::RefCell<StageStats>> =
        Rc::new(std::cell::RefCell::new(StageStats::default()));

    let rtt_sink = {
        let rtt = rtt_stats.clone();
        let wire = wire_stats.clone();
        echoes.clone().for_each(move |e, _t| {
            let Some(rtt_total) = e.t_client_recv.checked_sub(e.t_client_send) else {
                return;
            };
            let resident = e.stamps[8].saturating_sub(e.stamps[0]);
            rtt.borrow_mut().record(rtt_total);
            wire.borrow_mut().record(rtt_total.saturating_sub(resident));
            log::debug!(
                "echo session={} seq={} rtt={} resident={} wire={}",
                session_hex(&e.session),
                e.client_seq,
                rtt_total,
                resident,
                rtt_total.saturating_sub(resident),
            );
        })
    };
    let echo_counter = echoes.count();

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

    let mut nodes: Vec<Rc<dyn Node>> = vec![
        pub_orders,
        pub_fills,
        inbound_report,
        span_sink,
        rtt_sink,
        exporter.register("latency_e2e_active_sessions", active_gauge),
        exporter.register("latency_e2e_admitted_total", admitted_gauge),
        exporter.register("latency_e2e_rejected_total", rejected_gauge),
        exporter.register("latency_e2e_echoes_total", echo_counter),
    ];
    nodes.extend(register_stage_metrics(&exporter, &inbound_stats));
    nodes.extend(register_stage_stats(&exporter, "rtt_total", &rtt_stats));
    nodes.extend(register_stage_stats(&exporter, "wire_rtt", &wire_stats));

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

/// Register p50 / p99 / count gauges for a single `StageStats` handle
/// (used for the cross-domain `rtt_total` and `wire_rtt` histograms).
fn register_stage_stats(
    exporter: &PrometheusExporter,
    prefix: &str,
    stats: &Rc<std::cell::RefCell<StageStats>>,
) -> Vec<Rc<dyn Node>> {
    let tick = ticker(Duration::from_secs(1));
    let p50 = {
        let s = stats.clone();
        tick.produce(move || s.borrow().quantile_ns(0.5))
    };
    let p99 = {
        let s = stats.clone();
        tick.produce(move || s.borrow().quantile_ns(0.99))
    };
    let count = {
        let s = stats.clone();
        tick.produce(move || s.borrow().count)
    };
    vec![
        exporter.register(format!("latency_e2e_{prefix}_p50_ns"), p50),
        exporter.register(format!("latency_e2e_{prefix}_p99_ns"), p99),
        exporter.register(format!("latency_e2e_{prefix}_count_total"), count),
    ]
}

// Compile-time sanity: the iceoryx2 payload type is zero-copy sendable.
const _: fn() = || {
    fn assert_zc<T: ZeroCopySend>() {}
    assert_zc::<Traced<RoundTrip, RoundTripLatency>>();
};
