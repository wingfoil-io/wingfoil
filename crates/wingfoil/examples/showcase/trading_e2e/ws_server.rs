//! End-to-end latency demo — WebSocket server edge (wingfoil).
//!
//! Pipeline (this binary):
//!
//! ```text
//!   browser ── WS ──► ws_server ── iceoryx2 ──► fix_gw            (outbound)
//!   browser ◄── WS ── ws_server ◄── iceoryx2 ── fix_gw            (inbound)
//!   browser ── WS ──► ws_server                                    (echo)
//! ```
//!
//! Stamps `ws_recv` and `ws_publish` on the way out, `ws_sub_recv` and
//! `ws_send` on the way back. Enforces a global cap on concurrent sessions
//! and auto-expires each session after 60 s — protects the single LMAX
//! session living in `fix_gw` and keeps a public deployment bounded.
//!
//! A port of the legacy `legacy/wingfoil/examples/latency_e2e/ws_server.rs` onto the
//! wingfoil engine. The pipeline shape, the stamp stages, the metric names and the
//! wire types are unchanged; only the wiring is wingfoil-idiomatic — a
//! `GraphBuilder` plus the adapter extension traits instead of legacy's free
//! functions and explicit node vector.
//!
//! # Run (after starting fix_gw)
//!
//! ```sh
//! cargo run --manifest-path crates/wingfoil/Cargo.toml --release --example trading_e2e_ws_server \
//!   --features "web-tls,iceoryx2,prometheus,otlp" -- \
//!   --addr 0.0.0.0:8080 [--no-precise] \
//!   [--tls-cert /etc/wingfoil/tls/cert.pem --tls-key /etc/wingfoil/tls/key.pem]
//! ```
//!
//! Passing `--tls-cert`/`--tls-key` (or setting `WINGFOIL_TLS_CERT` /
//! `WINGFOIL_TLS_KEY`) switches the server to HTTPS + WSS on the same port. The
//! browser client (`static/app.js`) auto-detects scheme from
//! `location.protocol`, so no client-side config flips are needed.

#[path = "shared.rs"]
mod shared;

use std::cell::RefCell;
use std::collections::HashMap;
use std::path::PathBuf;
use std::rc::Rc;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use iceoryx2::prelude::ZeroCopySend;
use wingfoil::adapters::iceoryx2::{Iceoryx2SinkOps, iceoryx2_sub};
use wingfoil::adapters::otlp::{OtlpAttributeBuffer, OtlpConfig, OtlpSpanOps};
use wingfoil::adapters::prometheus::{PrometheusExporter, PrometheusSinkOps};
use wingfoil::adapters::web::{CodecKind, WebBurstSinkOps, WebServer, web_sub};
use wingfoil::latency::{
    Latency, LatencyBurstStreamOps, LatencyReportOps, LatencyStats, StageStats, Traced,
};
use wingfoil::prelude::*;
use wingfoil::{NanoTime, RunFor, RunMode};

use shared::{
    EchoFrame, FillFrame, OrderFrame, RoundTrip, RoundTripLatency, SVC_FILLS, SVC_ORDERS,
    SessionId, TOPIC_ECHO, TOPIC_FILLS, TOPIC_ORDERS, env_string, env_u64, pin_current_from_env,
    precise_stamps_enabled, round_trip_latency, session_hex,
};

/// The traced payload that rides shared memory in both directions.
type Fill = Traced<RoundTrip, RoundTripLatency>;

// ── Session registry ──────────────────────────────────────────────────────
//
// Lives in an `Arc<Mutex<…>>` because the Prometheus gauges read it from
// the graph thread while the order pipeline writes to it from the same
// thread — and the mutex additionally covers the (rare) case of the web
// adapter's consumer thread touching it. Every order frame touches this
// once; contention is negligible.

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

/// Lock the session registry, propagating a poisoned lock as a panic (a
/// poisoned lock means another thread panicked while holding it).
fn lock_sessions(sessions: &Mutex<Sessions>) -> std::sync::MutexGuard<'_, Sessions> {
    sessions.lock().expect("sessions mutex poisoned")
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

    let static_dir: PathBuf = std::env::var("WINGFOIL_STATIC_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|_| {
            PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("examples/showcase/trading_e2e/static")
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
    log::info!("session_cap={session_cap} ttl={session_ttl}s precise={precise}");

    let sessions = Arc::new(Mutex::new(Sessions::new(session_cap, session_ttl)));

    let g = GraphBuilder::new();

    // ── Outbound leg ─────────────────────────────────────────────────────
    //
    // Burst-shaped throughout. `collapse()` here — which is what this used to
    // do — keeps only the last frame of each burst, so two orders submitted in
    // one graph cycle became one, and the other browser waited forever for a
    // fill that was never requested.
    let orders_in = web_sub::<OrderFrame>(&g, &server, TOPIC_ORDERS)?;
    let traced_orders = {
        let s = sessions.clone();
        orders_in.map_filter(move |frames: &Burst<OrderFrame>| {
            let now_ns: u64 = NanoTime::now().into();
            let mut sessions = lock_sessions(&s);
            let admitted: Burst<Fill> = frames
                .iter()
                .filter(|frame| {
                    let admit = sessions.admit(&frame.session, now_ns);
                    if !admit {
                        log::warn!(
                            "rejected order session={} seq={} (cap reached)",
                            session_hex(&frame.session),
                            frame.client_seq,
                        );
                    }
                    admit
                })
                .map(|o| {
                    Fill::new(RoundTrip {
                        session: o.session,
                        client_seq: o.client_seq,
                        qty: o.qty,
                        side: o.side,
                        t_client_send: o.t_client_send,
                        ..Default::default()
                    })
                })
                .collect();
            let any = !admitted.is_empty();
            (admitted, any)
        })
    }
    .stamp_each_if::<round_trip_latency::ws_recv>(!precise)
    .stamp_precise_each_if::<round_trip_latency::ws_recv>(precise)
    .stamp_each_if::<round_trip_latency::ws_publish>(!precise)
    .stamp_precise_each_if::<round_trip_latency::ws_publish>(precise);

    let _pub_orders = traced_orders.iceoryx2_pub(SVC_ORDERS);

    // ── Inbound leg ──────────────────────────────────────────────────────
    let fills_in = iceoryx2_sub::<Fill>(&g, RunMode::RealTime, SVC_FILLS)?
        .stamp_each_if::<round_trip_latency::ws_sub_recv>(!precise)
        .stamp_precise_each_if::<round_trip_latency::ws_sub_recv>(precise)
        .stamp_each_if::<round_trip_latency::ws_send>(!precise)
        .stamp_precise_each_if::<round_trip_latency::ws_send>(precise);

    // Burst-shaped report: every fill in the burst contributes a sample.
    // Collapsing first would have thrown away the other fills' latency
    // measurements entirely — and biased the histogram, since the fills lost
    // are exactly the ones that arrived while the graph was busy.
    let (_inbound_report, inbound_stats) = fills_in.latency_report(true);

    // OTLP trace export — one parent span + one child per hop, per fill.
    // The attribute extractor pushes session.id and client_seq onto the
    // parent span so the Grafana dashboard can filter by session (which
    // Prometheus can't do without blowing up cardinality).
    let otlp_endpoint = env_string("WINGFOIL_OTLP_ENDPOINT", "http://localhost:4318");
    log::info!("otlp trace export → {otlp_endpoint}");
    let _span_sink = fills_in.otlp_spans(
        "roundtrip",
        OtlpConfig {
            endpoint: otlp_endpoint,
            service_name: "wingfoil-trading-e2e".into(),
        },
        |t: &Fill, attrs: &mut OtlpAttributeBuffer| {
            attrs.add("session.id", session_hex(&t.payload.session));
            attrs.add("client_seq", t.payload.client_seq as i64);
            attrs.add("side", if t.payload.side == 0 { "buy" } else { "sell" });
            attrs.add("filled_qty", t.payload.filled_qty as i64);
        },
    )?;

    let fill_frames = fills_in.map(|ts: &Burst<Fill>| {
        ts.iter()
            .map(|t| {
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
            })
            .collect::<Burst<FillFrame>>()
    });
    // `web_pub_each`, not `web_pub_bursts`: one frame per fill, byte-identical
    // to what the scalar `web_pub` produced, so `static/app.js` and the
    // `@wingfoil/client` subscription need no change.
    let _pub_fills = fill_frames.web_pub_each(&server, TOPIC_FILLS)?;

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

    let echoes = web_sub::<EchoFrame>(&g, &server, TOPIC_ECHO)?;

    let rtt_stats: Rc<RefCell<StageStats>> = Rc::new(RefCell::new(StageStats::default()));
    let wire_stats: Rc<RefCell<StageStats>> = Rc::new(RefCell::new(StageStats::default()));

    let _rtt_sink = {
        let rtt = rtt_stats.clone();
        let wire = wire_stats.clone();
        echoes.for_each(move |frames: &Burst<EchoFrame>| {
            for e in frames.iter() {
                let Some(rtt_total) = e.t_client_recv.checked_sub(e.t_client_send) else {
                    continue;
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
            }
            Ok(())
        })
    };

    // ── Prometheus metrics ───────────────────────────────────────────────
    let exporter = PrometheusExporter::new(&metrics_addr);
    let metrics_port = exporter.serve()?;
    log::info!("prometheus on http://{metrics_addr}/metrics (bound :{metrics_port})");

    // Sum the burst lengths rather than counting ticks: one cycle can carry
    // several echoes, and `count()` would report cycles-that-had-an-echo. The
    // old spelling was `echoes.map(|_| ()).count()`, which was only ever right
    // because `collapse` had already discarded the rest of each burst.
    let _echo_counter = echoes
        .map(|frames: &Burst<EchoFrame>| frames.len() as u64)
        .fold(0u64, |total: &mut u64, n: &u64| *total += *n)
        .prometheus_gauge(&exporter, "trading_e2e_echoes_total");

    let tick_1s = g.ticker(Duration::from_secs(1));
    let _active_gauge = {
        let s = sessions.clone();
        tick_1s
            .map(move |_: &()| lock_sessions(&s).active.len() as u64)
            .prometheus_gauge(&exporter, "trading_e2e_active_sessions")
    };
    let _admitted_gauge = {
        let s = sessions.clone();
        tick_1s
            .map(move |_: &()| lock_sessions(&s).admitted_total)
            .prometheus_gauge(&exporter, "trading_e2e_admitted_total")
    };
    let _rejected_gauge = {
        let s = sessions.clone();
        tick_1s
            .map(move |_: &()| lock_sessions(&s).rejected_total)
            .prometheus_gauge(&exporter, "trading_e2e_rejected_total")
    };

    register_stage_metrics(&g, &exporter, &inbound_stats);
    register_stage_stats(&g, &exporter, "rtt_total", &rtt_stats);
    register_stage_stats(&g, &exporter, "wire_rtt", &wire_stats);

    // Pin AFTER all eagerly-spawned adapter workers (web server, Prometheus
    // exporter) are running so they keep the default affinity mask. The
    // pinned mask applies only to the graph cycle thread (this one).
    pin_current_from_env("WINGFOIL_PIN_GRAPH");

    g.build().run(RunMode::RealTime, RunFor::Forever)?;
    Ok(())
}

// ── Per-stage Prometheus gauges ──────────────────────────────────────────
//
// The Prometheus adapter has no native label support, so we register one
// gauge per (stage × statistic) tuple. The Grafana dashboard groups them
// back together via metric-name regex.

fn register_stage_metrics<L: Latency + 'static>(
    g: &GraphBuilder,
    exporter: &PrometheusExporter,
    stats: &Rc<RefCell<LatencyStats<L>>>,
) {
    let names = L::stage_names();
    for i in 1..L::N {
        let stage = format!("{}__{}", names[i - 1], names[i]);
        let tick = g.ticker(Duration::from_secs(1));
        let _p50 = {
            let st = stats.clone();
            tick.map(move |_: &()| st.borrow().stages[i].quantile_ns(0.5))
                .prometheus_gauge(exporter, format!("trading_e2e_{stage}_p50_ns"))
        };
        let _p99 = {
            let st = stats.clone();
            tick.map(move |_: &()| st.borrow().stages[i].quantile_ns(0.99))
                .prometheus_gauge(exporter, format!("trading_e2e_{stage}_p99_ns"))
        };
        let _count = {
            let st = stats.clone();
            tick.map(move |_: &()| st.borrow().stages[i].count)
                .prometheus_gauge(exporter, format!("trading_e2e_{stage}_count_total"))
        };
    }
}

/// Register p50 / p99 / count gauges for a single [`StageStats`] handle
/// (used for the cross-domain `rtt_total` and `wire_rtt` histograms).
fn register_stage_stats(
    g: &GraphBuilder,
    exporter: &PrometheusExporter,
    prefix: &str,
    stats: &Rc<RefCell<StageStats>>,
) {
    let tick = g.ticker(Duration::from_secs(1));
    let _p50 = {
        let s = stats.clone();
        tick.map(move |_: &()| s.borrow().quantile_ns(0.5))
            .prometheus_gauge(exporter, format!("trading_e2e_{prefix}_p50_ns"))
    };
    let _p99 = {
        let s = stats.clone();
        tick.map(move |_: &()| s.borrow().quantile_ns(0.99))
            .prometheus_gauge(exporter, format!("trading_e2e_{prefix}_p99_ns"))
    };
    let _count = {
        let s = stats.clone();
        tick.map(move |_: &()| s.borrow().count)
            .prometheus_gauge(exporter, format!("trading_e2e_{prefix}_count_total"))
    };
}

// Compile-time sanity: the iceoryx2 payload type is zero-copy sendable.
const _: fn() = || {
    fn assert_zc<T: ZeroCopySend>() {}
    assert_zc::<Fill>();
};
