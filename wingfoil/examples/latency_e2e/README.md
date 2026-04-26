# latency_e2e — end-to-end latency demo

A multi-process wingfoil pipeline that stamps wall-clock timestamps at every
hop on the way out and back, accumulates per-hop histograms, and renders a
live per-session dashboard in the browser.

```
browser ── WebSocket ──► ws_server ── iceoryx2 ──► fix_gw ── FIX/TLS ──► LMAX
   ▲                          ▲                       │                      │
   │                          │                       ▼                      │
   │                          └──── iceoryx2 ◄──── fix_gw ◄──── FIX/TLS ◄───┘
   └────────── WebSocket ─────┘
```

Nine stamp stages, in order: `ws_recv → ws_publish → gw_recv → gw_price →
fix_send → fix_recv → gw_publish → ws_sub_recv → ws_send`.

## Layout

```
examples/latency_e2e/
  shared.rs          payload + latency schema, env-var helpers
  ws_server.rs       binary — WS edge, iceoryx2 pub/sub, session cap, prometheus
  fix_gw.rs          binary — iceoryx2 pub/sub, LMAX MD subscribe, pricing, fill
  static/            browser client (single index.html + app.js, uPlot via CDN)
  prometheus/        prometheus scrape config
  grafana/           provisioned datasource + dashboard
  docker-compose.yml grafana + prometheus stack
```

## Run it

Two binaries, one browser tab. The order doesn't matter — the iceoryx2
service auto-discovers.

**Note on stale iceoryx2 shared memory:** If you've previously killed the examples
with `SIGKILL` (e.g., `pkill -9`), orphaned shared memory files may persist in `/dev/shm/`.
Run the cleanup script before starting:

```bash
bash wingfoil/examples/latency_e2e/cleanup_stale_shm.sh
```

Then:

```bash
# Required — fix_gw opens two TLS FIX sessions to LMAX London Demo
# (market data + order routing). The binary refuses to start without these.
export LMAX_USERNAME=...
export LMAX_PASSWORD=...

# Terminal 1
cargo run --release --example latency_e2e_fix_gw \
  --features "fix,iceoryx2-beta" -- --precise

# Terminal 2
cargo run --release --example latency_e2e_ws_server \
  --features "web,iceoryx2-beta,prometheus,otlp" -- --addr 0.0.0.0:8080 --precise

# Terminal 3 (operator stack — Prometheus + Tempo + Grafana, auto-provisioned)
docker compose -f wingfoil/examples/latency_e2e/docker-compose.yml up -d
```

Then open `http://localhost:8080` and click **start**. The page shows
three panels simultaneously: a live in-page chart (this session's
per-hop latency in real time), an embedded Grafana iframe showing
aggregate p50/p99 across all sessions (Prometheus), and below that
the per-session trace waterfall (Tempo), pre-filtered to this
browser's UUID via `?var-session=…`.

## Intra-cycle vs cycle-start stamps

`stamp::<S>` reads `GraphState::wall_time()` (one load, snapped at cycle
start). `stamp_precise::<S>` reads `GraphState::wall_time_precise()` (a
fresh TSC, ~5–10 ns extra cost). Either way, stages that share an engine
cycle would otherwise collide on identical timestamps; precise mode gives
each stamp a distinct value.

Toggle either way:

```bash
# CLI
cargo run --example latency_e2e_ws_server -- --precise
# or env
WINGFOIL_PRECISE_STAMPS=1 cargo run --example latency_e2e_ws_server
```

The `.stamp_if::<S>(enabled)` / `.stamp_precise_if::<S>(enabled)`
operators compile to a no-op when disabled — zero runtime cost when off.

## Session cap and auto-expiry

`ws_server` admits up to `WINGFOIL_SESSION_CAP` (default 8) concurrent
sessions, each living `WINGFOIL_SESSION_SECS` (default 60). Orders past
the cap are dropped server-side and a warning is logged. This caps load
on the LMAX session and bounds Prometheus cardinality.

## Three observability views, one browser tab

Per-session metric labels would explode Prometheus cardinality (one
unique label per UUID) — so we route the signal to the right tool:

| View | Where | Storage | Filtered to my session? |
|------|-------|---------|-------------------------|
| Live per-hop chart | in-page uPlot | in-memory only | Yes — browser renders fills carrying its own UUID |
| Aggregate per-hop p50/p99 | Grafana | Prometheus (low cardinality) | No — aggregated across all sessions |
| Per-session trace waterfall | Grafana (embedded iframe on the page) | Tempo (high cardinality OK) | Yes — `$session` template var pre-filled from page |

Every completed fill is exported as an OTLP trace by `ws_server`:
one parent span `roundtrip` covering the full `stamps[0..N-1]` window
plus one child span per hop. `session.id`, `client_seq`, `side`, and
`filled_qty` are attached as span attributes so TraceQL can search by
any of them. Tempo handles the cardinality natively (object-store
backend, trace-ID-keyed) — the storage economics of traces are built
for this, whereas Prometheus would OOM.

The `otlp_spans` stream operator that drives this is part of the
wingfoil `otlp` adapter (see `wingfoil/src/adapters/otlp/traces.rs`).
It's generic over any `Stream<P>` where `P: HasLatency` and takes a
closure for attribute extraction — reusable for any wingfoil pipeline,
not just this demo.

## How `fix_gw` matches orders to fills

Two FIX sessions, one HashMap, no custom node — the matcher is composed
from stock wingfoil primitives:

```
orders ──► price ──► stamp(fix_send) ──┬──► for_each: inject NewOrderSingle
                                        │
                                        └──► map(MatcherEvent::Order) ─┐
                                                                       ├─► combine ─► fold ─► map_filter ─► stamp(fix_recv) ─► stamp(gw_publish) ─► iceoryx2_pub
order_session.data ─► map_filter(Exec) ────────────────────────────────┘
```

`combine(vec![order_events, exec_events])` emits a
`Burst<MatcherEvent>` per cycle containing whichever of the two
upstreams ticked — zero, one, or both. `fold` carries a
`RefCell<HashMap<ClOrdID, Traced<…>>>` in its captured state and walks
the burst in order: Order events `park(t)`; ExecReport events
`remove(id)`, merge fill data from tags 31/32 (or 0/0 on
reject/cancel so the round-trip still closes), and set `*last = Some`.
The downstream `MapFilterStream` drops the Nones.

ClOrdID is `"<sessionHex>-<seq>"` — unique by construction. Orders go
out as IOC limits (TimeInForce=3, OrdType=2) priced at the opposite
touch, so every order produces a terminal ExecutionReport (Fill,
partial-fill-then-cancel, or reject) within milliseconds. No timeouts
needed.

## Cross-clock RTT — single-clock arithmetic, no NTP

`performance.now()` (browser) and `NanoTime::now()` (server) use
different epochs, but we never compare across them. Every delta we
care about is a subtraction within *one* clock frame:

```
rtt_total   = T4 - T1                    (client clock)
resident    = stamps[8] - stamps[0]      (server clock)
wire_rtt    = rtt_total - resident       (same units; just minus)
```

The browser records `T1 = nowNs()` at order submit and `T4 = nowNs()`
at fill receipt; the server stamps `stamps[0] = ws_recv` and
`stamps[8] = ws_send`. The browser posts all four back on
`TOPIC_ECHO`; the server aggregates `rtt_total` and `wire_rtt` into
two `StageStats` histograms exposed via Prometheus
(`latency_e2e_rtt_total_{p50,p99,count}_ns` and
`latency_e2e_wire_rtt_…`). No offset estimation, no convergence
heuristic, no symmetric-path assumption. The only thing we don't
split is inbound vs outbound wire legs — they're lumped into
`wire_rtt` together.

## Ports

| Service | Port |
|---------|------|
| `ws_server` HTTP + WebSocket | 8080 |
| `ws_server` Prometheus exporter | 9091 |
| Prometheus | 9090 |
| Tempo (HTTP API + OTLP ingest) | 3200, 4318 |
| Grafana | 3000 |

All overridable via the env vars documented at the top of each binary.

## Pinning the graph thread to a core

Both binaries run their hot graph cycle on the main thread. Set
`WINGFOIL_PIN_GRAPH` to a comma-separated core list to pin it (Linux only;
no-op elsewhere):

```bash
# Single core
WINGFOIL_PIN_GRAPH=2 cargo run --release --example latency_e2e_ws_server ...

# Multi-core set (kernel may schedule across any of these)
WINGFOIL_PIN_GRAPH=2,3 cargo run --release --example latency_e2e_fix_gw ...
```

The adapter worker threads (`wingfoil-web`, FIX session, iceoryx2 sub) are
mostly I/O-blocked, so we don't pin them individually. If you want to keep
them off the hot core too, isolate the core at boot (`isolcpus=2`) and run
the rest of the process on the housekeeping cores via `taskset` — the
explicit `WINGFOIL_PIN_GRAPH` call still wins on the graph thread.

## Pre-commit

```bash
cargo fmt --all
cargo clippy --workspace --all-targets --exclude wingfoil-python -- -D warnings
```
