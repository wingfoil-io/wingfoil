# Introspection and visualisation

How a user *sees* the graph they wired — its shape, what it costs, and what is
flowing through it.

This is the plan the port deliberately left a hole for. Legacy wingfoil's
`Graph::export` wrote a GML file and nothing else; it was dropped rather than
ported (deviation register **C6**, cutover-plan row **2.1**, port-plan Phase 5)
with the reasoning recorded each time: *we want a designed introspection story,
not a same-shape port of a debug-only helper.* This document is that design.

**Status: step 1 has landed** (`crates/wingfoil/src/introspect.rs`). Steps 2-4
are scoped here, not built.

---

## The shape of the problem

Three quite different things get bundled under "visualise the graph", and they
have almost nothing in common in cost, in mechanism, or in who wants them:

| | Needs a run? | Cost to collect | Who wants it |
|---|---|---|---|
| **Structure** — nodes, edges, tiers | no | zero | everyone, constantly |
| **Cost** — per-node timing, tick rates | yes | *significant*, see below | anyone tuning |
| **Values** — what is actually flowing | yes | per-node opt-in | anyone debugging |

Keeping them apart is the main design decision here. A structural snapshot must
stay free enough to call from a unit test; a profile must never be on by
accident; values cannot be collected generically at all (see
[Constraint: values](#constraint-values-cannot-be-generic)). Bundling them into
one "devtools" feature would make the cheap thing expensive and the expensive
thing invisible.

---

## What the engine already gives us

Most of the raw material exists. That is why this is a modest amount of work
rather than a subsystem.

- **Topology, complete, at wiring time.** `NodeRt` (`interp.rs`) carries
  `active_ups`, `passive_ups`, `activation` and a `label`. Crucially it keeps
  active and passive edges *separate*, which is the fact a wingfoil user most
  wants a picture for and the one legacy's GML flattened away.
- **A per-node span already exists.** `instrument-cycle-node` emits a
  `tracing` span per node per cycle carrying index and label, behind its own
  feature, off by default. The profiling story starts from that shape.
- **A transport, already shipping.** The `web` adapter is axum + WebSocket with
  `serve_static`; `wingfoil-wire-types` is the shared schema crate;
  `wingfoil-wasm` decodes it; `@wingfoil/client` wraps it with Solid/Svelte/Vue
  bindings, and `js/examples/solid-dashboard` is a working dashboard. A live UI
  is a *consumer* of infrastructure that already exists, not new plumbing.
- **Cross-process latency.** `Traced<T, L>` / `latency_stages!` / `LatencyStats`
  already measure staged latency across process hops.
- **Determinism.** `HistoricalFrom` consults no clock: `begin_cycle` sets
  `time = next.max(time + 1)` from the schedule alone. This is what makes step 4
  possible at all.

---

## Step 1 — structural snapshot ✅ landed

`GraphSnapshot`, from `GraphBuilder::snapshot()` (before build, repeatable,
non-consuming) or `Runner::snapshot()` (after build, and after a run).

```rust
let snap = g.snapshot();
assert_eq!(snap.sources().count(), 1);
println!("{snap}");            // text form; `<-` active, `<~` passive
```

Renders to text, Mermaid, Graphviz DOT, JSON and GML. Reads no clock, no value
slot, runs no op — so it cannot perturb a measurement.

Why five formats and not one: they land in genuinely different places. Mermaid
renders inline on GitHub, so a graph can go in a README or a pull request. DOT
lays out a wide DAG better than anything else. Text is what you want from a
failing assertion. JSON is for the next step's consumers. GML is continuity for
anyone who was using legacy's `export` with yEd or Gephi.

This supersedes `Graph::export` rather than restoring it: the topology is a
*value* you can assert on, active and passive edges are distinguished, and each
node carries its `Activation`.

**Deliberately excluded**: values, tick counts, timings. Structure only.

---

## Step 2 — per-node cost

### Why a flame graph is the wrong metaphor, slightly

A flame graph represents nested call stacks. A wingfoil cycle is a *flat drain*
of a dirty list in index order — nothing nests except a `nested()` island. A
literal flame graph of one cycle would be a bar chart in a costume.

The shape that answers the question users actually have is a flame graph
**rooted at the triggering source**: *"one market-data tick costs 3.2µs across
14 nodes, 2.1µs of it in this `fold`."* That genuinely is a tree — it is the
tick propagation frontier — and the dispatch loop already knows who marked whom
dirty, so the parent/child relation costs nothing extra to record. Island
boundaries nest inside it naturally.

So: two views over one dataset.

- **Self-time per node**, aggregated — the flat profile. Answers "what is hot".
- **Cost per source-tick**, as a tree — the flame. Answers "what does an
  incoming event cost me", which is the question a trading graph is tuned
  against.

### Measurement cost is the binding constraint

This repository treats a 24ns clock read as significant enough to build a lazy
`Cell` around (see the two-clocks section of `CLAUDE.md`). Two timer reads per
node per cycle is not a rounding error when a whole cycle can be sub-microsecond
— it would be measuring the profiler.

Therefore:

1. **Behind its own feature, off by default**, exactly as every `instrument-*`
   feature is. A default build carries no branch and no dependency.
2. **Sampled, not exhaustive.** Profile 1-in-N cycles. The tail matters, so the
   sampling must be periodic-in-cycles rather than random-in-time, and the
   sample count must be reported alongside.
3. **Honest output.** A profiled run's absolute numbers are not a clean run's,
   and the report must say so rather than letting someone quote them.
4. **Folded stacks first, UI never necessarily.** Emit the
   `inferno`/`speedscope` folded-stack format and existing tooling renders it.
   A bespoke renderer is weeks of work and worse. A `--profile` flag writing a
   file is most of the value.

### Surface sketch

```rust
// Feature `profile`. Collect into the Runner, drain after the run.
let mut runner = g.build();
runner.profile(ProfileCfg::sampled(64));          // 1 cycle in 64
runner.run(RunMode::RealTime, RunFor::Forever)?;
let report = runner.profile_report();             // per-node + per-source-tick
report.write_folded("graph.folded")?;             // speedscope / inferno
println!("{report}");                             // flat table, ranked
```

The report should be joinable against a `GraphSnapshot` by node index, which is
what makes step 3 a rendering job rather than a data-collection job.

**Open question**: whether the timer is `quanta` (already a dependency, TSC-
backed) or raw `rdtsc` with a calibration pass. `quanta`'s `Instant::now` is
what `NanoTime::now` already uses, so starting there costs nothing to try and
the bench harness (`benches/`) can settle it.

---

## Step 3 — live browser devtools

Only worth building once steps 1 and 2 define the data model. Then it is
mostly a rendering job.

- **Wire**: a reserved control topic `"$introspect"` alongside the web adapter's
  existing `"$ctrl"`, carrying a `GraphSnapshot` once on connect and profile
  deltas periodically thereafter.
- **Schema**: the snapshot and profile types move (or are mirrored) into
  `wingfoil-wire-types`, which exists for exactly this and survives cutover.
  Step 1's types are already `Serialize + Deserialize` with owned `String`
  labels specifically so that move is not a breaking change.
- **Server**: one call — `devtools(&server, &runner)` — registering the topic.
  Static assets ship through the existing `serve_static`.
- **Client**: `@wingfoil/client` already decodes the envelope and has framework
  bindings; `js/examples/solid-dashboard` is the precedent to follow.

Features worth having, roughly in order of value:

- **Colour by tier** — interpreted / inside a `compiled()` box / inside an
  island. This is the main performance lever a user has and it is currently
  invisible in both the source and any picture.
- **Edge weight by tick rate**, colour by burst size. Fan-out storms and nodes
  ticking far more than expected show up immediately.
- **Drill in/out of islands** — an island is one node outside and a whole graph
  inside, so the UI should collapse and expand on exactly that boundary.
- **"Why did this tick?"** — click a value, get the upstream chain that caused
  it *this cycle*. Falls straight out of the dirty-list propagation.

---

## Step 4 — record and replay

The one that is hard to copy, and the reason to keep the others honest.

Because `HistoricalFrom` is deterministic and source-driven, **recording only
the source inputs is enough to replay the entire graph bit-exact.** Every node's
value at cycle N is reproducible from a recording proportional to input volume,
not to graph size.

That buys three things:

- A **scrubbable timeline** in the step-3 UI, where stepping backwards is real
  rather than a buffer of recent values.
- **Incident forensics**: a production graph ships a small recording, and the
  failure is replayed locally under a debugger, cycle by cycle.
- **Breakpoints and watch expressions** — pause the kernel on a predicate, step
  one cycle. Sound only in historical mode, which is the mode you would debug
  in anyway.

Prerequisite: every source must be recordable. `ticker`/`constant` are
reproducible from config alone; `channel` already carries timestamps and
replays; `external`/`poll` are realtime-only and would need a capture tap. That
last one is the real work in this step.

---

## Constraint: values cannot be generic

Structure and timing can be collected for every node automatically. **Values
cannot.**

`NodeRt` is deliberately non-generic — every node lives in one `Vec` and the
value slot is captured *inside* the `cycle` closure. There is no way to read an
arbitrary node's value without a serializer registered at wiring time, and that
would mean a `Serialize` bound the fluent API does not have and should not gain
universally (it would exclude every non-serializable payload from the graph).

So the v1 shape is an **opt-in per-stream tap**:

```rust
let mid = quotes.map(mid_price).tap("mid_price");
```

Structure and timing everywhere; values where you asked for them. The wire
format should carry taps as a sparse map keyed by node index so that a future
automatic tap — say, one `nitro!` emits when it can prove `T: Serialize` — is an
addition rather than a break.

---

## What this is not

- **Not a replacement for Prometheus/OTLP.** Those adapters and the Grafana
  stack in `examples/telemetry/` are the *production* observability tier. This
  is the *development* tier: higher fidelity, lower durability, aimed at one
  person looking at one graph. Per-node metrics worth keeping in production
  should go out through the existing exporters, not through a browser socket.
- **Not `logged`.** `StreamOps::logged` is a per-value debug tap through the
  `log` crate and stays as it is — it answers "what is this one stream doing"
  without any of the above.
- **Not on by default, ever.** Steps 2-4 each carry a runtime cost. Step 1 is
  the only part that is free, and it is the only part that is unconditional.

---

## Sequencing

Steps 1 and 2 are independently valuable with no UI at all, and together they
define the data model that makes step 3 a rendering exercise instead of an
architecture exercise. Step 4 depends on step 3 for its interface but on nothing
else.

| Step | Depends on | Ships without a UI? |
|---|---|---|
| 1 — structural snapshot ✅ | — | yes |
| 2 — per-node cost | 1 (for node identity) | yes, as folded stacks |
| 3 — live devtools | 1, 2 | no |
| 4 — record/replay | 3 for the scrubber | partly (replay is useful headless) |

## Related

- `docs/deviation-register.md` — **C6**, the recorded drop this supersedes.
- `docs/cutover-plan.md` — row **2.1**.
- `docs/wingfoil-architecture.md` — the two clocks, and why measurement cost is
  treated as seriously as it is here.
- `crates/wingfoil/examples/core/introspect/` — step 1, runnable.
