# Introspection and visualisation

How a user *sees* the graph they wired — its shape, what it costs, and what is
flowing through it.

This is the plan the port deliberately left a hole for. Legacy wingfoil's
`Graph::export` wrote a GML file and nothing else; it was dropped rather than
ported (deviation register **C6**, cutover-plan row **2.1**, port-plan Phase 5)
with the reasoning recorded each time: *we want a designed introspection story,
not a same-shape port of a debug-only helper.* This document is that design.

**Status: the structural snapshot has landed**
(`crates/wingfoil/src/introspect.rs`). Everything else here is scoped, not
built.

---

## The governing decision: dev and prod are not the same product

The single most important thing in this document. Every other choice follows
from it, and conflating the two is what makes this look harder than it is.

|  | Development / debugging | Production monitoring |
|---|---|---|
| Lifetime | ephemeral, minutes | continuous, months |
| Scope | one process, one human looking | N processes, alerting, retention |
| Topology | automatic | automatic (static, cheap) |
| **Rates** | **all nodes, automatic** | **opt-in `.monitor("name")`** + fixed aggregates |
| **Values** | **all nodes, automatic** | **none** |
| Transport | WebSocket → bespoke UI | Prometheus scrape → Grafana |
| Cost tolerance | irrelevant (feature off in release) | strict, bounded, predictable |

**Automatic-everything is correct for dev and wrong for prod**, and not only for
values — for rates too. Three reasons auto-exporting per-node metrics to a
production monitoring system does not work:

1. **Node identity is not stable.** A node's index is wiring-order dependent:
   insert one node upstream and every index below it shifts. Auto-derived metric
   names (`wingfoil_node_47_map_ticks`) would silently re-point at a different
   node on an unrelated refactor, taking every dashboard and alert with them.
   Anything you page on needs a name a human chose.
2. **Graph size is not bounded in the cases that matter.** The dynamism examples
   wire a price book over instruments that come and go — node count scales with
   instrument count, and nodes are spliced in and removed at runtime. 5,000
   instruments × 10 nodes, times pod labels, *with churn*, is the Prometheus
   high-cardinality antipattern rather than a rounding error.
3. **Signal-to-noise.** Nobody alerts on "node 47 (`Map`) rate". They alert on
   order rate, reject rate, feed staleness, book crossed. Two hundred
   `Map`/`Filter`/`Fold` series bury the five that matter.

Dev inverts all three: identity is stable because the process is the one you are
looking at right now, the graph is whatever you are debugging, and a human is
reading it rather than an alert rule.

---

## What the engine already gives us

Most of the raw material exists. That is why this is a modest amount of work
rather than a subsystem.

- **Topology, complete, at wiring time.** `NodeRt` (`interp.rs`) carries
  `active_ups`, `passive_ups`, `activation` and a `label`, keeping active and
  passive edges *separate* — the fact a wingfoil user most wants a picture for,
  and the one legacy's GML flattened away.
- **A per-node tick flag, written every cycle.** The dispatch loop already does
  `self.ticked.borrow_mut()[i] = did` (`interp.rs:3074`), and already maintains
  aggregate counters `node_visits` / `layer_visits` (`interp.rs:3112`). Per-node
  tick *counts* are an increment beside a write that already happens.
- **An `Rc`-sharing pattern for getting engine state out.** `ticked` is an
  `Rc<RefCell<Vec<bool>>>` shared between `Builder`, `Runner` and nodes. Any
  counter vector can ride the same seam, which lets an ordinary in-graph node
  read it — no busy-poll, no special engine hook.
- **A WebSocket transport, already shipping.** The `web` adapter is axum +
  WebSocket with `serve_static`; `wingfoil-wire-types` is the shared schema
  crate; `wingfoil-wasm` decodes it; `@wingfoil/client` wraps it with
  Solid/Svelte/Vue bindings, and `js/examples/solid-dashboard` is a working
  dashboard. Notably `web_pub` works in **both** run modes, so this watches a
  backtest replay, not only a live run.
- **A metrics transport, already shipping.** The `prometheus` adapter serves
  `GET /metrics` with a lock-free `ArcSwapOption` slot per registered stream —
  never a lock on the graph thread — plus a Grafana + Prometheus compose stack
  in `examples/adapters/telemetry/`. It is realtime-only and a no-op under
  `HistoricalFrom`, which is the right behaviour for a production exporter.
- **Determinism.** `HistoricalFrom` consults no clock: `begin_cycle` sets
  `time = next.max(time + 1)` from the schedule alone. This is what makes
  record/replay possible at all.

---

## Landed: the structural snapshot

`GraphSnapshot`, from `GraphBuilder::snapshot()` (before build, repeatable,
non-consuming) or `Runner::snapshot()` (after build, and after a run).

```rust
let snap = g.snapshot();
assert_eq!(snap.sources().count(), 1);
println!("{snap}");            // text form; `<-` active, `<~` passive
```

Renders to text, Mermaid, Graphviz DOT, JSON and GML. Reads no clock, no value
slot, runs no op — so it cannot perturb a measurement.

Five formats because they land in different places: Mermaid renders inline on
GitHub, DOT lays out a wide DAG best, text is what you want from a failing
assertion, JSON feeds the two tracks below, GML is continuity for anyone using
legacy's `export` with yEd or Gephi.

This supersedes `Graph::export` rather than restoring it: the topology is a
*value* you can assert on, active and passive edges are distinguished, and each
node carries its `Activation`.

**Deliberately structure only** — no values, no tick counts, no timings.

---

## Track A — production monitoring

Three tiers, smallest blast radius first.

### A1. Always-on graph-level aggregates (fixed cardinality)

O(1) series regardless of graph size. This is the "is the engine keeping up"
signal, and it is what you page on:

```
wingfoil_cycles_total
wingfoil_node_visits_total          # Runner::node_visits already exists
wingfoil_cycle_duration_seconds     # histogram
wingfoil_source_ticks_total{source="quotes"}
```

Nearly free — the engine already maintains the two visit counters.

### A2. Opt-in per-node monitoring

```rust
let mid = quotes.map(mid_price).monitor("mid_price");
```

Stable identity because the name is chosen, not derived. Operator-meaningful.
Same ergonomic as the existing `prometheus_gauge`, but exporting rate and
latency rather than value. A real graph marks perhaps 5–20 nodes.

### A3. `export_metrics_all()` — the escape hatch

Everything, explicitly, documented as high-cardinality. For a soak test or a
staging box for an hour — never a default.

### The Grafana picture

The DAG still gets rendered, by Grafana rather than a bespoke UI. Grafana's
Node Graph panel consumes a nodes frame plus an edges frame, which is close to
what `GraphSnapshot` already is — so `to_grafana_nodegraph()` is a small
addition beside the existing renderers.

The result is arguably better than a live-everything view: the topology is the
static map, annotated with live numbers only where you marked. Context from the
picture, deliberate metrics on top.

**Values never go to prod monitoring** — high volume, often sensitive, and not
what you monitor on.

**Why not the WebSocket devtools in prod**: a socket into one process cannot
survive a restart, aggregate across processes, retain history, or alert. Prod
needs all four, and the `web` adapter would have to stay compiled in with a live
listener.

---

## Track B — development devtools

Here everything is automatic, because the constraints that forbid it in prod do
not apply.

**Trigger** — one line, or none:

```rust
runner.devtools(&server)?;                  // --features devtools
// or, touching no code at all:
// WINGFOIL_DEVTOOLS=127.0.0.1:8080 cargo run
```

**Transport** — a reserved `"$introspect"` topic alongside the web adapter's
existing `"$ctrl"`, carrying the `GraphSnapshot` once on connect and deltas
thereafter. Schema types move (or are mirrored) into `wingfoil-wire-types`,
which exists for exactly this and survives cutover; the landed snapshot types
are already `Serialize + Deserialize` with owned `String` labels so that move is
not a breaking change. UI ships through the existing `serve_static`.

### B1. Per-node rates, automatic

A `tick_counts: Rc<RefCell<Vec<u64>>>` incremented beside the existing
`ticked[i] = did` write, shared through the same seam as `ticked`, and scraped
by an ordinary 10Hz `ticker` node that publishes deltas. The sampler being a
normal graph node is what keeps this off the hot path — no busy-poll, no engine
hook.

Aggregating before publishing is about volume, not architecture: it makes the
payload independent of throughput. For a modest graph, publishing per-tick is
fine, and throttling is the ordinary `sample`-against-a-slow-ticker you would
apply to any stream.

**This counter is dev-only.** With the opt-in prod design above, production
never needs it, so it lives behind the `devtools` feature rather than being a
general engine change.

### B2. Per-node values, automatic

The interesting one, and the reason "no manual wire-up" is achievable.

Slots are centralised (`slots: Vec<Rc<dyn Any>>`) and `Builder::slot::<T>()`
downcasts — so a value is reachable *if you know `T`*, and `dyn Any` does not
give you serialization. But `T` **is** statically known at every
`new_slot::<T>()` call site, and those sites are generated by
`#[op(build = …)]`.

So the macro emits a conditional serializer via **autoref specialization** (the
stable-Rust `(&&value).probe()` trick, resolving to a `Serialize` impl when one
exists and a blanket fallback otherwise). No `Serialize` bound reaches the
public API, so non-serializable payloads still flow through the graph exactly as
they do now.

Fallback ladder, so every node shows something:

| node payload | UI shows |
|---|---|
| `T: Serialize` | the value |
| `T: Debug` only | the `Debug` string |
| neither | type name + tick rate |

This is the fiddliest part of the design — it lives in the proc macro and needs
care across the three `nitro!` tiers — but it is what buys "no manual wire-up",
so it is where the effort belongs.

### B3. Views worth having

- **Colour by tier** — interpreted / inside a `compiled()` box / inside an
  island. The main performance lever a user has, currently invisible.
- **Edge weight by tick rate**, colour by burst size — fan-out storms show up
  immediately.
- **Drill in/out of islands** — collapse and expand on exactly that boundary.
- **"Why did this tick?"** — click a value, get the upstream chain that caused
  it this cycle. Falls out of the dirty-list propagation.

---

## Per-node profiling (either track, feature-gated, off by default)

### A flame graph rooted at the source, not at a call stack

A flame graph represents nested call stacks. A wingfoil cycle is a *flat drain*
of a dirty list in index order — nothing nests except a `nested()` island. A
literal flame graph of one cycle would be a bar chart in a costume.

The shape that answers the real question is a flame graph **rooted at the
triggering source**: *"one market-data tick costs 3.2µs across 14 nodes, 2.1µs
of it in this `fold`."* That genuinely is a tree — the tick propagation frontier
— and the dispatch loop already knows who marked whom dirty, so the parent/child
relation costs nothing extra. Two views over one dataset:

- **Self-time per node**, aggregated — "what is hot".
- **Cost per source-tick**, as a tree — "what does an incoming event cost me",
  which is the question a trading graph is tuned against.

### Measurement cost is the binding constraint

This repository treats a 24ns clock read as significant enough to build a lazy
`Cell` around. Two timer reads per node per cycle is not a rounding error when a
cycle can be sub-microsecond — it would be measuring the profiler. Therefore:

1. **Own feature, off by default**, as every `instrument-*` feature is.
2. **Sampled, not exhaustive** — 1 cycle in N, periodic-in-cycles rather than
   random-in-time so the tail survives, with the sample count reported.
3. **Honest output** — a profiled run's absolute numbers are not a clean run's,
   and the report must say so.
4. **Folded stacks first** — emit the `inferno`/`speedscope` format and existing
   tooling renders it. A `--profile` flag writing a file is most of the value; a
   bespoke renderer is weeks of work and worse.

**Open question**: whether the timer is `quanta` (already a dependency, TSC-
backed, what `NanoTime::now` uses) or raw `rdtsc` with calibration. Starting
with `quanta` costs nothing to try and the bench harness can settle it.

---

## Record and replay

The one that is hard to copy. Because `HistoricalFrom` is deterministic and
source-driven, **recording only the source inputs replays the entire graph
bit-exact** — a recording proportional to input volume, not graph size.

- A **scrubbable timeline** in the devtools UI, where stepping backwards is real
  rather than a buffer of recent values.
- **Incident forensics**: a production graph ships a small recording; the
  failure is replayed locally under a debugger, cycle by cycle.
- **Breakpoints and watch expressions** — pause on a predicate, step one cycle.
  Sound only in historical mode, which is where you would debug anyway.

Prerequisite: every source must be recordable. `ticker`/`constant` are
reproducible from config alone; `channel` already carries timestamps and
replays; `external`/`poll` are realtime-only and need a capture tap. That last
one is the real work here.

---

## The limitation that does not go away

`compiled()` and `nested()` islands have no node table — the state lives in
local variables and that absence *is* the speed. An island renders as **one
opaque box**; a fully `compiled()` graph as nothing at all.

This is inherent, not deferred work: you cannot have both "the optimiser erased
the node boundaries" and "show me the node boundaries."

It bites **harder in production**, because production is where you would
compile. The honest position: islands report an aggregate only, full resolution
is available when running interpreted, and the docs say so plainly rather than
pretending otherwise.

---

## Two smaller sharp edges

- **Historical mode makes "rate" ambiguous.** Ticks per second of *engine* time
  is meaningful in a backtest (and enormous); ticks per second of wall time
  measures your machine. Any UI must label which, and should default to engine
  time.
- **Busy-poll (`ALWAYS`) graphs** cycle back-to-back at ~10M/sec. Counter
  increments are fine; anything per-cycle that touches a clock is not.

---

## What this is not

- **Not a replacement for the existing exporters.** The `prometheus`/`otlp`
  adapters and the Grafana stack in `examples/adapters/telemetry/` *are* the
  production tier — Track A extends them, it does not compete with them.
- **Not `logged`.** `StreamOps::logged` is a per-value debug tap through the
  `log` crate and stays as it is.
- **Not on by default.** The snapshot is the only free part, and the only part
  that is unconditional.

---

## Build order

Each step is independently useful and the cheap ones come first.

| # | Step | Track | Engine change? |
|:--:|---|---|---|
| ✅ | Structural snapshot | both | no |
| 1 | `to_grafana_nodegraph()` + dashboard JSON | A | **no** — `snapshot()` already has the data |
| 2 | Graph-level aggregates + `.monitor()` opt-in | A | small |
| 3 | Sampled profiler → folded stacks | either | feature-gated |
| 4 | Dev counters + `devtools()` + browser UI | B | feature-gated |
| 5 | Record/replay + scrubber | B | source capture taps |

**Production monitoring is complete after step 2**, and steps 1–2 together are a
fraction of step 4. If the Grafana route proves sufficient, a bespoke UI may
only ever be needed for debugging — which is exactly what Track B is scoped as.

## Related

- `docs/deviation-register.md` — **C6**, the recorded drop this supersedes.
- `docs/cutover-plan.md` — row **2.1**.
- `docs/wingfoil-architecture.md` — the two clocks, and why measurement cost is
  treated as seriously as it is here.
- `crates/wingfoil/examples/core/introspect/` — the landed snapshot, runnable.
- `crates/wingfoil/examples/adapters/telemetry/` — the existing Grafana +
  Prometheus stack Track A builds on.
