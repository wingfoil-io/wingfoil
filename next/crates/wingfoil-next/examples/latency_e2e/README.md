# latency_e2e — end-to-end latency demo (wingfoil-next)

A multi-process wingfoil-next pipeline that stamps wall-clock timestamps at
every hop on the way out and back, accumulates per-hop histograms, and renders a
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

A port of the classic `wingfoil/examples/latency_e2e` onto the next engine. It
is the largest single consumer of next's adapter surface — `web` (+ TLS),
`iceoryx2`, `fix`, `prometheus` and `otlp` all in one graph — plus the Phase-5
latency infrastructure (`latency_stages!` + `Traced<T, L>` +
`.stamp_precise::<Stage>()` + `latency_report`) across two processes. The
classic copy keeps shipping untouched until Phase 7 and remains the parity
oracle. Deviations are listed at the bottom.

## Layout

```
examples/latency_e2e/
  shared.rs          payload + latency schema, env-var helpers
  ws_server.rs       binary — WS edge, iceoryx2 pub/sub, session cap, prometheus
  fix_gw.rs          binary — iceoryx2 pub/sub, LMAX MD subscribe, pricing, fill
  static/            browser client (single index.html + app.js, uPlot via CDN)
  prometheus/        prometheus scrape config
  grafana/           provisioned datasource + dashboard
  tempo/             tempo config (trace storage)
  docker-compose.yml grafana + prometheus + tempo stack
  Dockerfile.*       five images (both binaries + the three observability ones)
  DOCKER_BUILD.md    building / pushing those images
  cleanup_stale_shm.sh
  pulumi/
    fargate/         always-on AWS Fargate stack (cheap public demo)
    ec2-spot/        always-on EC2 Spot stack (baked AMI)
    baremetal/       on-demand EC2 bare-metal stack (perf showcase)
```

## Run it

Two binaries, one browser tab. The order doesn't matter — the iceoryx2
service auto-discovers.

**Note on stale iceoryx2 shared memory:** If you've previously killed the examples
with `SIGKILL` (e.g., `pkill -9`), orphaned shared memory files may persist in `/dev/shm/`.
Run the cleanup script before starting:

```bash
bash next/crates/wingfoil-next/examples/latency_e2e/cleanup_stale_shm.sh
```

Then:

```bash
# Required — fix_gw opens two TLS FIX sessions to LMAX London Demo
# (market data + order routing). The binary refuses to start without these.
export LMAX_USERNAME=...
export LMAX_PASSWORD=...

# Terminal 1
cargo run -p wingfoil-next --release --example latency_e2e_fix_gw \
  --features "fix,iceoryx2"

# Terminal 2 — local dev: plain HTTP (skip --tls-cert/--tls-key).
# For HTTPS / WSS, pass --tls-cert / --tls-key (or set
# WINGFOIL_TLS_CERT / WINGFOIL_TLS_KEY); the cargo feature is the same.
cargo run -p wingfoil-next --release --example latency_e2e_ws_server \
  --features "web-tls,iceoryx2,prometheus,otlp" -- --addr 0.0.0.0:8080

# Terminal 3 (operator stack — Prometheus + Tempo + Grafana, auto-provisioned)
docker compose -f next/crates/wingfoil-next/examples/latency_e2e/docker-compose.yml up -d
```

Then open `http://localhost:8080` and click **start**. The page shows
three panels simultaneously: a live in-page chart (this session's
per-hop latency in real time), an embedded Grafana iframe showing
aggregate p50/p99 across all sessions (Prometheus), and below that
the per-session trace waterfall (Tempo), pre-filtered to this
browser's UUID via `?var-session=…`.

## Intra-cycle vs cycle-start stamps

`.stamp::<S>()` reads `Ctx::wall_time()` (one load, snapped at cycle
start). `.stamp_precise::<S>()` reads `Ctx::wall_time_precise()` (a
fresh TSC, ~5–10 ns extra cost). Either way, stages that share an engine
cycle would otherwise collide on identical timestamps; precise mode gives
each stamp a distinct value.

Precise stamps are **on by default** in this example — without them,
hops that fire in the same cycle (e.g. `ws_recv → ws_publish`,
`gw_recv → gw_price → fix_send`, `fix_recv → gw_publish`,
`ws_sub_recv → ws_send`) measure 0 ns and disappear from the log-scale
chart. To opt out:

```bash
# CLI
cargo run -p wingfoil-next --example latency_e2e_ws_server -- --no-precise
# or env (also accepts false / no / off)
WINGFOIL_PRECISE_STAMPS=0 cargo run -p wingfoil-next --example latency_e2e_ws_server
```

The `.stamp_if::<S>(enabled)` / `.stamp_precise_if::<S>(enabled)`
operators return the upstream unchanged when disabled — no node is
inserted into the graph, so it costs nothing when off.

Latency ops are **fluent/interpreted-only by design** (deviation-register
entry C7): a stamp's stage is a compile-time *type* parameter, which does not
map onto the `nitro!` / `compiled()` value-dispatch table. This example is
therefore wired entirely through the fluent layer, exactly as its classic
counterpart is.

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
wingfoil-next `otlp` adapter (see
`next/crates/wingfoil-next/src/adapters/otlp.rs`). It's generic over any
`Stream<P>` where `P: HasLatency` and takes a closure for attribute
extraction — reusable for any wingfoil pipeline, not just this demo.

## How `fix_gw` matches orders to fills

Two FIX sessions, one HashMap, no custom node — the matcher is composed
from stock wingfoil-next combinators:

```
orders ──► price ──► stamp(fix_send) ──┬──► for_each: inject NewOrderSingle
                                        │
                                        └──► map(MatcherEvent::Order) ─┐
                                                                       ├─► combine ─► fold ─► map_filter ─► stamp(fix_recv) ─► stamp(gw_publish) ─► iceoryx2_pub
order_session.data ─► map_filter(Exec) ─────────────────────────────────┘
```

`g.combine(&[order_events, exec_events])` emits a
`Burst<MatcherEvent>` per cycle containing whichever of the two
upstreams ticked — zero, one, or both. `fold` carries a
`RefCell<HashMap<ClOrdID, Traced<…>>>` in its captured state and walks
the burst in order: Order events `park(t)`; ExecReport events
`remove(id)`, merge fill data from tags 31/32 (or 0/0 on
reject/cancel so the round-trip still closes), and set `*last = Some`.
The downstream `map_filter` drops the Nones.

The pricing step is `orders.join_passive(&book, …)` — next's spelling of
classic's `bimap(Dep::Active(orders), Dep::Passive(book), …)`: an inbound order
triggers the pricing, the book's current value is read without triggering it.

ClOrdID is `"<sessionHex(last 8)>-<seq>"` — unique by construction. Orders go
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
WINGFOIL_PIN_GRAPH=2 cargo run -p wingfoil-next --release --example latency_e2e_ws_server ...

# Multi-core set (kernel may schedule across any of these)
WINGFOIL_PIN_GRAPH=2,3 cargo run -p wingfoil-next --release --example latency_e2e_fix_gw ...
```

The adapter worker threads (the web server, the FIX session, the iceoryx2 sub)
are mostly I/O-blocked, so we don't pin them individually. If you want to keep
them off the hot core too, isolate the core at boot (`isolcpus=2`) and run the
rest of the process on the housekeeping cores via `taskset` — the explicit
`WINGFOIL_PIN_GRAPH` call still wins on the graph thread.

## Deviations from classic

The pipeline shape, the nine stamp stages, the wire types, the iceoryx2
service names, the Prometheus metric names, the env-var surface and the CLI
flags are all **unchanged**, so a classic browser client and a classic Grafana
dashboard work against the next binaries untouched. What differs is wiring
idiom, plus one packaging fact:

1. **Wiring is next-idiomatic.** A `GraphBuilder` replaces classic's explicit
   `Vec<Rc<dyn Node>>` + `Graph::new(nodes, …)`: every wired node is already in
   the graph, so there is no node vector to assemble and no
   `fix_md.data.as_node()` keep-alive. The adapter entry points follow next's
   conventions — sources take `(&g, run_mode, …)` and return `Result`
   (`web_sub`, `iceoryx2_sub`, `fix_connect_tls`), sinks are extension traits
   (`.iceoryx2_pub(..)`, `.web_pub(..)`, `.prometheus_gauge(..)`,
   `.otlp_spans(..)`). None of this is an [adapter
   deviation](../../../../docs/deviation-register.md) introduced here — it is
   the already-registered D1/B2 shape of the ported adapters.
2. **Combinator spellings.** `join_passive` for classic `bimap(Dep::Active,
   Dep::Passive)`; `map_filter` for `filter_map` / `MapFilterStream`;
   `tick.map(|_| …)` for `tick.produce(…)`; `stream.map(|_| ()).count()` for
   `stream.count()` (next's `count()` is defined on `Stream<()>`);
   `stream.prometheus_gauge(&exporter, name)` for
   `exporter.register(name, stream)`. `otlp_spans` takes
   `(span_name, config, attrs)` where classic took `(config, span_name, attrs)`.
   All are behaviour-preserving renames from the ported ops/adapters.
3. **`.lock().expect("sessions mutex poisoned")`** rather than classic's
   `.lock().unwrap()` — the repo's error-handling policy. A poisoned lock still
   propagates the panic, deliberately.
4. **The CI workflows are not repointed.** `build-latency-e2e-ami.yml`,
   `build-latency-e2e-images.yml` and `deploy-latency-e2e.yml` still build and
   deploy the **classic** copy by path. Repointing them at this twin is
   cutover-time work (cutover-plan row 5.2), not part of the port; until then
   the Dockerfiles and Pulumi stacks here are built manually per
   [`DOCKER_BUILD.md`](DOCKER_BUILD.md).

Unlike the `latency` example port — which had to add `#[type_name(...)]` to
both payload types to work around an iceoryx2 `IncompatibleTypes` abort, and
gave its subscriber an optional run duration so the teardown report is
reachable — **neither classic defect exists here**: `latency_e2e`'s `shared.rs`
already pins both type names, and both binaries run forever by design (the
`latency_report` sink feeds live Prometheus gauges rather than a teardown
print), so there is nothing to fix.

## Pre-commit

```bash
cargo fmt --all
cargo lint        # default features
cargo lint-all    # all features
```
