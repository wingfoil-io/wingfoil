// End-to-end latency demo — WebSocket server edge.
//
// Pipeline (this binary):
//   browser ── WS ──► ws_server                                    (control: start/stop)
//                        │
//                        └── per-session ticker subgraphs ── iceoryx2 ──► fix_gw   (outbound)
//   browser ◄── WS ── ws_server ◄── iceoryx2 ── fix_gw            (inbound)
//
// On `CONTROL_START`, `OrderGenerator` calls `state.add_upstream` to wire
// in a `ticker(period)` subgraph keyed by the session UUID — that ticker
// IS the rate. On `STOP`, `state.remove_node` tears it down. A "live
// rate update" (browser tweaks Hz while running) is just del-then-add:
// the user sees a smooth rate change with no stop/restart.
//
// Stamps `ws_recv` and `ws_publish` inline in the generator, `ws_sub_recv`
// and `ws_send` on the inbound stamp ops. Session cap + TTL backstop
// browser disconnects so the LMAX session in fix_gw stays bounded.
//
// Run (after starting fix_gw):
//   cargo run --example latency_e2e_ws_server \
//     --features "web-tls,iceoryx2,prometheus,otlp,dynamic-graph-beta" -- \
//     --addr 0.0.0.0:8080 [--no-precise] \
//     [--tls-cert /etc/wingfoil/tls/cert.pem --tls-key /etc/wingfoil/tls/key.pem]

#[path = "shared.rs"]
mod shared;

use std::collections::HashMap;
use std::path::PathBuf;
use std::rc::Rc;
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

// ── OrderGenerator ────────────────────────────────────────────────────────
//
// Custom MutableNode that owns one ticker subgraph per active session.
// Adding/removing a session is `state.add_upstream` / `state.remove_node`,
// so each session's ticker IS the source of truth for its rate — no
// per-cycle poll, no shared mutex.

struct ActiveSession {
    ticker: Rc<dyn Node>,
    side: u8,
    alt_next: u8,
    qty: u64,
    client_seq: u64,
    admitted_at: NanoTime,
}

struct OrderGenerator {
    control: Rc<dyn Stream<ControlFrame>>,
    gc: Rc<dyn Node>,
    active: HashMap<SessionId, ActiveSession>,
    cap: usize,
    ttl: Duration,
    precise: bool,
    value: Burst<Traced<RoundTrip, RoundTripLatency>>,
}

impl OrderGenerator {
    fn new(
        control: Rc<dyn Stream<ControlFrame>>,
        gc: Rc<dyn Node>,
        cap: usize,
        ttl: Duration,
        precise: bool,
    ) -> Self {
        Self {
            control,
            gc,
            active: HashMap::new(),
            cap,
            ttl,
            precise,
            value: burst![],
        }
    }

    fn evict(&mut self, state: &mut GraphState, id: &SessionId) {
        if let Some(s) = self.active.remove(id) {
            state.remove_node(s.ticker);
        }
    }

    fn now(&self, state: &mut GraphState) -> NanoTime {
        if self.precise {
            state.wall_time_precise()
        } else {
            state.wall_time()
        }
    }
}

#[node(output = value: Burst<Traced<RoundTrip, RoundTripLatency>>)]
impl MutableNode for OrderGenerator {
    fn upstreams(&self) -> UpStreams {
        UpStreams::new(
            vec![self.control.clone().as_node(), self.gc.clone()],
            vec![],
        )
    }

    fn cycle(&mut self, state: &mut GraphState) -> anyhow::Result<bool> {
        self.value.clear();

        if state.ticked(self.control.clone().as_node()) {
            let frame = self.control.peek_value();
            // START on an existing session = rate update via del+add.
            self.evict(state, &frame.session);
            match frame.action {
                CONTROL_START if self.active.len() >= self.cap => {
                    log::warn!(
                        "rejected start session={} (cap {} reached)",
                        session_hex(&frame.session),
                        self.cap,
                    );
                }
                CONTROL_START => {
                    let period = Duration::from_nanos(1_000_000_000 / frame.rate_hz.max(1) as u64);
                    let tick = ticker(period);
                    state.add_upstream(tick.clone(), true, true);
                    self.active.insert(
                        frame.session,
                        ActiveSession {
                            ticker: tick,
                            side: frame.side,
                            alt_next: 0,
                            qty: frame.qty,
                            client_seq: 0,
                            admitted_at: state.wall_time(),
                        },
                    );
                }
                CONTROL_STOP => {} // already evicted
                _ => {}
            }
        }

        if state.ticked(self.gc.clone()) {
            let cutoff = state.wall_time() - NanoTime::from(self.ttl);
            let expired: Vec<SessionId> = self
                .active
                .iter()
                .filter(|(_, s)| s.admitted_at < cutoff)
                .map(|(id, _)| *id)
                .collect();
            for id in &expired {
                self.evict(state, id);
            }
        }

        // Active sessions whose ticker fired this cycle each emit one order.
        // Two tickers can coincide; iceoryx2_pub iterates the Burst.
        let ids: Vec<SessionId> = self.active.keys().copied().collect();
        for id in ids {
            let ticker_node = self.active[&id].ticker.clone();
            if !state.ticked(ticker_node) {
                continue;
            }
            let s = self.active.get_mut(&id).unwrap();
            let side = if s.side == SIDE_ALT {
                let v = s.alt_next;
                s.alt_next ^= 1;
                v
            } else {
                s.side
            };
            s.client_seq += 1;
            let mut t = Traced::<RoundTrip, RoundTripLatency>::new(RoundTrip {
                session: id,
                client_seq: s.client_seq,
                qty: s.qty,
                side,
                ..Default::default()
            });
            let recv: u64 = self.now(state).into();
            round_trip_latency::ws_recv::stamp(&mut t.latency, recv);
            let publish: u64 = self.now(state).into();
            round_trip_latency::ws_publish::stamp(&mut t.latency, publish);
            self.value.push(t);
        }

        Ok(!self.value.is_empty())
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
    let session_ttl = Duration::from_secs(env_u64("WINGFOIL_SESSION_SECS", 60));
    let precise = precise_stamps_enabled();

    let static_dir: PathBuf = std::env::var("WINGFOIL_STATIC_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|_| {
            PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("examples/latency_e2e/static")
        });

    // TLS material is opt-in: missing cert/key keeps the server on plain
    // HTTP/WS. Pulumi user_data wires both args so deployed boxes serve HTTPS.
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
    log::info!(
        "session_cap={session_cap} ttl={:?} precise={precise}",
        session_ttl,
    );

    let control = web_sub::<ControlFrame>(&server, TOPIC_CONTROL).collapse::<ControlFrame>();
    let gc_tick = ticker(Duration::from_secs(1));
    let orders =
        OrderGenerator::new(control, gc_tick, session_cap, session_ttl, precise).into_stream();
    let pub_orders = iceoryx2_pub(orders, SVC_ORDERS);

    // ── Inbound leg ──────────────────────────────────────────────────────
    let fills_in = iceoryx2_sub::<Traced<RoundTrip, RoundTripLatency>>(SVC_FILLS)
        .collapse::<Traced<RoundTrip, RoundTripLatency>>()
        .stamp_if::<round_trip_latency::ws_sub_recv>(!precise)
        .stamp_precise_if::<round_trip_latency::ws_sub_recv>(precise)
        .stamp_if::<round_trip_latency::ws_send>(!precise)
        .stamp_precise_if::<round_trip_latency::ws_send>(precise);

    let (inbound_report, inbound_stats) = fills_in.latency_report(true);

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

    let exporter = PrometheusExporter::new(&metrics_addr);
    let metrics_port = exporter.serve()?;
    log::info!("prometheus on http://{metrics_addr}/metrics (bound :{metrics_port})");

    let mut nodes: Vec<Rc<dyn Node>> = vec![pub_orders, pub_fills, inbound_report, span_sink];
    nodes.extend(register_stage_metrics(&exporter, &inbound_stats));

    // Pin AFTER adapter workers spawn so only the graph thread is pinned.
    pin_current_from_env("WINGFOIL_PIN_GRAPH");

    Graph::new(nodes, RunMode::RealTime, RunFor::Forever).run()?;
    Ok(())
}

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

const _: fn() = || {
    fn assert_zc<T: ZeroCopySend>() {}
    assert_zc::<Traced<RoundTrip, RoundTripLatency>>();
};
