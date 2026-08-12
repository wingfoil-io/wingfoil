# trading_e2e — end-to-end trading stack (wingfoil)

A multi-process wingfoil pipeline that carries an order from a browser to a live
venue and the fill back again: WebSocket in, shared memory across processes,
FIX/TLS to LMAX, a top-of-book folded from live market data, session admission
and expiry, Prometheus metrics, OTLP traces, provisioned Grafana dashboards, and
Pulumi stacks for three deployment shapes.

Latency instrumentation runs through all of it — nine stamp stages, per-hop
histograms and a live per-session chart — but it is one of the things this
example does, not the whole of it. (It was called `latency_e2e` until that
became misleading; the names it emits moved with it, see
[below](#the-emitted-namespace-moved-with-the-rename).)

```
browser ── WebSocket ──► ws_server ── iceoryx2 ──► fix_gw ── FIX/TLS ──► LMAX
   ▲                          ▲                       │                      │
   │                          │                       ▼                      │
   │                          └──── iceoryx2 ◄──── fix_gw ◄──── FIX/TLS ◄───┘
   └────────── WebSocket ─────┘
```

Nine stamp stages, in order: `ws_recv → ws_publish → gw_recv → gw_price →
fix_send → fix_recv → gw_publish → ws_sub_recv → ws_send`.

A port of the legacy `legacy/wingfoil/examples/latency_e2e` onto the wingfoil engine. It
is the largest single consumer of wingfoil's adapter surface — `web` (+ TLS),
`iceoryx2`, `fix`, `prometheus` and `otlp` all in one graph — plus the Phase-5
latency infrastructure (`latency_stages!` + `Traced<T, L>` +
`.stamp_precise::<Stage>()` + `latency_report`) across two processes. The
legacy copy keeps shipping untouched until Phase 7 and remains the parity
oracle. Deviations are listed at the bottom.

## Layout

```
examples/showcase/trading_e2e/
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
bash crates/wingfoil/examples/showcase/trading_e2e/cleanup_stale_shm.sh
```

Then:

```bash
# Required — fix_gw opens two TLS FIX sessions to LMAX London Demo
# (market data + order routing). The binary refuses to start without these.
export LMAX_USERNAME=...
export LMAX_PASSWORD=...

# Terminal 1
cargo run --manifest-path crates/wingfoil/Cargo.toml --release --example trading_e2e_fix_gw \
  --features "fix,iceoryx2"

# Terminal 2 — local dev: plain HTTP (skip --tls-cert/--tls-key).
# For HTTPS / WSS, pass --tls-cert / --tls-key (or set
# WINGFOIL_TLS_CERT / WINGFOIL_TLS_KEY); the cargo feature is the same.
cargo run --manifest-path crates/wingfoil/Cargo.toml --release --example trading_e2e_ws_server \
  --features "web-tls,iceoryx2,prometheus,otlp" -- --addr 0.0.0.0:8080

# Terminal 3 (operator stack — Prometheus + Tempo + Grafana, auto-provisioned)
docker compose -f crates/wingfoil/examples/showcase/trading_e2e/docker-compose.yml up -d
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
cargo run --manifest-path crates/wingfoil/Cargo.toml --example trading_e2e_ws_server -- --no-precise
# or env (also accepts false / no / off)
WINGFOIL_PRECISE_STAMPS=0 cargo run --manifest-path crates/wingfoil/Cargo.toml --example trading_e2e_ws_server
```

The `_if` operators return the upstream unchanged when disabled — no node is
inserted into the graph, so it costs nothing when off. This example is
burst-shaped throughout (see [below](#nothing-here-is-burst-collapsed)), so the
spellings it actually uses are `.stamp_each_if::<S>(enabled)` and
`.stamp_precise_each_if::<S>(enabled)`.

`stamp` and `stamp_precise` reach **all three** `nitro!` expansions —
`interpreted()`, `compiled()` and `nested()`. That was deviation-register
entry C7 and it is closed: a stamp's stage is a compile-time *type*, which
`nitro!`'s value-dispatch cannot forward, so `#[op(explicit = S)]` gives each
forwarder a leading `PhantomData<S>` and the emission passes
`PhantomData::<the_stage>` — inference then resolves the stage from an
argument like any other. The burst-shaped twins this example actually uses
carry the same attribute and reach the same three tiers. All four are pinned in
`crates/wingfoil/tests/latency.rs` — `stamps_reach_the_compiled_tier`,
`stamps_reach_a_nested_island`, and the two `burst_stamps_*` cases —
and `tests/op_completeness.rs` records them as pinned there rather than in its
own blocks.

`latency_report` is the one latency op that stays interpreted-only, and
structurally rather than by omission: the sink's whole value is the
`Rc<RefCell<LatencyStats>>` handle it returns, and `compiled()` is
outputs-only, so the handle cannot escape.

What a graph this shape *cannot* do is compile whole: the adapters are the
point of it, and busy-poll (`ALWAYS`) sources and bursts are not expressible
in `compiled()` (deviation-register entry C4, a deliberate exclusion). The
shape C4 prescribes instead is "IO at the interpreted boundary + compiled
islands" — see the next section for how much of that this example can
actually take.

## Nothing here is burst-collapsed

Every ingest in this example is a `Stream<Burst<T>>` and **stays** one, all the
way to the sink. That is deliberate, and it is the one design rule to carry
away from this example if you carry nothing else.

The natural spelling for an adapter-fed pipeline is `.collapse::<T>()` — it
turns `Stream<Burst<T>>` into the `Stream<T>` every scalar combinator wants,
and this example used to open all six of its ingest paths with it. But
`collapse` ticks the burst's **last** item and discards the rest. A subscriber
drains everything queued into one burst, so multi-item bursts are not an edge
case: they are what happens whenever a producer outruns a graph cycle, i.e.
exactly under load. Collapsing an order, fill or control-message path is
therefore silent data loss that only appears when the system is busy — orders
that never reach the venue, fills whose round trip never closes, execution
reports that leave their order parked in the matcher forever.

So the pipeline is burst-shaped throughout, using the burst-aware forms:

| Instead of | Use |
|---|---|
| `.collapse()` then `.stamp::<S>()` | [`.stamp_each::<S>()`](../../../src/latency.rs) / `.stamp_precise_each::<S>()` |
| `.collapse()` then `.latency_report(..)` | `.latency_report(..)` — it has a `Stream<Burst<P>>` impl |
| `.collapse()` then `.otlp_spans(..)` | `.otlp_spans(..)` — resolves to the `Stream<Burst<P>>` impl |
| `.collapse()` then `.web_pub(..)` | `.web_pub_each(..)` — same wire format, one frame per value |
| `.collapse()` then `map`/`fold`/`for_each` | iterate the burst inside the closure |

`crates/wingfoil/tests/latency_bursts.rs` pins the difference: the same source
through the burst path samples every value, and through `collapse` loses two of
every three.

The clock is read **once per burst**, not once per value — a burst is one
instant's worth of values, so a per-value read would invent differences that do
not exist. `stamp_precise_each` still separates *stages* within a cycle, which
is what precise stamping is for.

## Compiled islands

There are none, and the reason is worth recording because it is not obvious.

The market-data top-of-book builder was briefly a `nitro!` island — a
`collapse` + `fold` pair behind one node. Removing the `collapse` (see above:
it was dropping market-data updates when a cycle carried several refreshes)
leaves a single `fold`, and an island around one node is strictly worse than
the node: the same one dyn call, plus the composite's boundary. So it went.

Every other hot chain in these two binaries — the admit / build / stamp chain
in `ws_server`, the pricing chain and the matcher in `fix_gw` — hits at least
one of three constraints. They are worth knowing before you reach for an island
in your own graph, because none is obvious from the tier documentation:

1. **A wiring function takes only `&Stream<T>` parameters.** Anything else is
   rejected at expansion (`stream parameters must be taken by reference`), so
   no runtime config and no shared handle can cross the boundary: not
   `precise`, not `max_md_age_ms`, not the `Arc<Mutex<Sessions>>` registry,
   not the `FixSender`, not the `PrometheusExporter`.
2. **`stamp_if` / `stamp_precise_if` have no `nitro!` forwarder**, and cannot
   have one — their semantic is *insert a node or don't*, a wiring-time
   branch, where an op only ever describes a cycle. Plain `stamp` and
   `stamp_precise` do work in all three tiers, but the `--no-precise` toggle
   is exactly the wiring-time branch that cannot go inside an island.
3. **The interior runs inside an `FnMut`**, so a `move` closure capturing
   per-graph state cannot be built there (`cannot move out of value, a
   captured variable in an FnMut closure`). That rules out the matcher, whose
   `RefCell<HashMap<ClOrdID, Fill>>` of parked orders is captured. Folding
   the map as the accumulator instead is not a workaround: `Fold`'s output
   *is* its accumulator, so it would clone the whole `HashMap` every tick.

Note that constraint 2 is now the binding one nearly everywhere: with the
pipeline burst-shaped, the stamps on every leg are `stamp_each_if(precise)` —
a wiring-time branch on a runtime flag, which is exactly what cannot cross into
an island.

If you do build one, three spellings inside a `nitro!` block differ from the
fluent original, all worth knowing:

* `collapse()` drops its turbofish — the forwarder resolves the element type
  by inference, and an explicit one collides with the `PhantomData` argument
  the forwarder already carries.
* Combinator arguments must be **literal closures**, not function paths. The
  macro takes the call-site argument tokens as the op's `Cfg`, so a named
  `fn` becomes part of the config *type* rather than something the op calls.
* The wiring function must take `g: &GraphBuilder` first, even when its
  interior wires no source of its own.

`crates/wingfoil/tests/island_collapse_fold.rs` still pins that island shape
against the same wiring done flat — values and tick times — even though this
example no longer mounts one.

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
wingfoil `otlp` adapter (see
`crates/wingfoil/src/adapters/otlp.rs`). It's generic over any
`Stream<P>` where `P: HasLatency` and takes a closure for attribute
extraction — reusable for any wingfoil pipeline, not just this demo.

## How `fix_gw` matches orders to fills

Two FIX sessions, one HashMap, no custom node — the matcher is composed
from stock wingfoil combinators:

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

The pricing step is `orders.join_passive(&book, …)` — wingfoil's spelling of
legacy's `bimap(Dep::Active(orders), Dep::Passive(book), …)`: an inbound order
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
(`trading_e2e_rtt_total_{p50,p99,count}_ns` and
`trading_e2e_wire_rtt_…`). No offset estimation, no convergence
heuristic, no symmetric-path assumption. The only thing we don't
split is inbound vs outbound wire legs — they're lumped into
`wire_rtt` together.

## The emitted namespace moved with the rename

The example was renamed from `latency_e2e`, and the names it *emits* moved
with it:

| What | Now |
|---|---|
| Prometheus metrics | `trading_e2e_*` |
| Prometheus job names | `trading_e2e_ws_server`, `trading_e2e_spot_watcher` |
| iceoryx2 services | `wingfoil/trading_e2e/{orders,fills}` |
| `#[type_name(...)]` pins | `wingfoil::trading_e2e::{RoundTrip,RoundTripLatency}` |
| OTLP service name | `wingfoil-trading-e2e` |
| Grafana dashboard UID / title | `wingfoil-trading-e2e` / *wingfoil trading end-to-end* |

Everything that consumes these names lives in this directory and moved in the
same commit — the provisioned dashboard's queries and TraceQL filter, the
scrape config, and the browser client's Grafana deep-link. Two consequences
are worth knowing before you upgrade a running deployment:

* **A Prometheus already scraping an older stack keeps the old series.** The
  `latency_e2e_*` and `trading_e2e_*` names are unrelated as far as Prometheus
  is concerned, so graphs will break at the cutover point rather than
  continue. For a demo stack the answer is to drop the old series; there is no
  in-place rename.
* **These binaries are no longer wire-compatible with the legacy twin.** The
  iceoryx2 service names and the `#[type_name(...)]` pins are both part of
  service identity, so a legacy `ws_server` and a wingfoil `fix_gw` will not
  see each other (iceoryx2 reports `IncompatibleTypes`). Run a matched pair.
  The legacy tree is deleted at cutover, so this is a temporary condition.

What did **not** move is the deployment infrastructure's own naming: the
Pulumi project names (`wingfoil-latency-{fargate,ec2-spot,baremetal}`), the
Packer AMI name (`wingfoil-latency-ec2-spot-*`) and the SSM parameter path
(`/wingfoil/latency-e2e/ec2-spot/ami_id`). Those identify *deployed state*
rather than anything the binaries emit — a Pulumi project name is part of
stack identity, so changing it orphans running stacks, and the SSM path has an
IAM grant scoped to it. Renaming them is a deploy-window operation, not a code
change.

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
WINGFOIL_PIN_GRAPH=2 cargo run --manifest-path crates/wingfoil/Cargo.toml --release --example trading_e2e_ws_server ...

# Multi-core set (kernel may schedule across any of these)
WINGFOIL_PIN_GRAPH=2,3 cargo run --manifest-path crates/wingfoil/Cargo.toml --release --example trading_e2e_fix_gw ...
```

The adapter worker threads (the web server, the FIX session, the iceoryx2 sub)
are mostly I/O-blocked, so we don't pin them individually. If you want to keep
them off the hot core too, isolate the core at boot (`isolcpus=2`) and run the
rest of the process on the housekeeping cores via `taskset` — the explicit
`WINGFOIL_PIN_GRAPH` call still wins on the graph thread.

## Deviations from legacy

The pipeline shape, the nine stamp stages, the wire types, the env-var surface
and the CLI flags are all **unchanged**, so a legacy browser client still works
against the wingfoil binaries untouched. What differs is wiring idiom, the
emitted namespace, and one packaging fact:

1. **Wiring is wingfoil-idiomatic.** A `GraphBuilder` replaces legacy's explicit
   `Vec<Rc<dyn Node>>` + `Graph::new(nodes, …)`: every wired node is already in
   the graph, so there is no node vector to assemble and no
   `fix_md.data.as_node()` keep-alive. The adapter entry points follow wingfoil's
   conventions — sources take `(&g, run_mode, …)` and return `Result`
   (`web_sub`, `iceoryx2_sub`, `fix_connect_tls`), sinks are extension traits
   (`.iceoryx2_pub(..)`, `.web_pub(..)`, `.prometheus_gauge(..)`,
   `.otlp_spans(..)`). None of this is an [adapter
   deviation](../../../../../docs/deviation-register.md) introduced here — it is
   the already-registered D1/B2 shape of the ported adapters.
2. **Combinator spellings.** `join_passive` for legacy `bimap(Dep::Active,
   Dep::Passive)`; `map_filter` for `filter_map` / `MapFilterStream`;
   `tick.map(|_| …)` for `tick.produce(…)`; `stream.map(|_| ()).count()` for
   `stream.count()` (wingfoil's `count()` is defined on `Stream<()>`);
   `stream.prometheus_gauge(&exporter, name)` for
   `exporter.register(name, stream)`. `otlp_spans` takes
   `(span_name, config, attrs)` where legacy took `(config, span_name, attrs)`.
   All are behaviour-preserving renames from the ported ops/adapters.
3. **`.lock().expect("sessions mutex poisoned")`** rather than legacy's
   `.lock().unwrap()` — the repo's error-handling policy. A poisoned lock still
   propagates the panic, deliberately.
4. **The CI workflows build this copy, and the package spec carries a
   version.** `build-trading-e2e-ami.yml`, `build-trading-e2e-images.yml` and
   `deploy-trading-e2e.yml` were repointed off the legacy copy ahead of the
   cutover. The Dockerfiles and the baremetal Pulumi stack build
   `-p wingfoil@9.0.0`, not `-p wingfoil`: legacy is an unconditional
   dev-dependency of this crate and examples link dev-dependencies, so both
   packages named `wingfoil` are in the graph and the bare spec is ambiguous.
   The version goes away with the legacy tree.
5. **The emitted namespace is `trading_e2e`, not `latency_e2e`** — metric
   names, Prometheus job names, iceoryx2 service names, `#[type_name(...)]`
   pins, the OTLP service name and the Grafana dashboard UID. This is the one
   deviation that is not source-only: it means a legacy Grafana dashboard no
   longer matches these metrics, and a legacy binary no longer shares an
   iceoryx2 service with a wingfoil one. See [the emitted
   namespace](#the-emitted-namespace-moved-with-the-rename).

Unlike the `latency` example port — which had to add `#[type_name(...)]` to
both payload types to work around an iceoryx2 `IncompatibleTypes` abort, and
gave its subscriber an optional run duration so the teardown report is
reachable — **neither legacy defect exists here**: `trading_e2e`'s `shared.rs`
already pins both type names, and both binaries run forever by design (the
`latency_report` sink feeds live Prometheus gauges rather than a teardown
print), so there is nothing to fix.

## Pre-commit

```bash
cargo fmt --all
cargo lint        # default features
cargo lint-all    # all features
```
