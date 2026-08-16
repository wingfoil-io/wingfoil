# Wingfoil architecture

Orientation for someone about to change the engine. Not an API reference —
that is the rustdoc — but the shape of the thing and the reasons behind the
handful of decisions that everything else follows from.

Read this before your first non-trivial change. It is roughly 20 minutes, and
it will save you from the two or three "why on earth is it like *that*"
moments that the code cannot explain on its own.

## What the library does

You describe a directed acyclic graph of data transformations once, and run it
either against live data or replayed over history, with identical semantics.
Nodes are pure computations; the engine owns their state, their scheduling and
their wiring.

```rust
let g = GraphBuilder::new();
let count = g.ticker(Duration::from_millis(10)).count();
let is_even = count.map(|n: &u64| n.is_multiple_of(2));
let total = count.filter(&is_even).fold(0u64, |acc, v| *acc += v);

let mut runner = g.build();
runner.run(RunMode::RealTime, RunFor::Cycles(100))?;
```

## The one decision everything follows from

**Node semantics are an associated function, not a method on an object.**

```rust
trait Op {
    type Cfg;              // construction-time config; closures live here
    type State;            // engine-owned mutable state
    type In<'a>;           // typed inputs, passed in per cycle
    type Out;              // the produced value
    const ACTIVATION: Activation;

    fn cycle(cfg: &mut Self::Cfg, state: &mut Self::State,
             input: Self::In<'_>, ctx: &mut Ctx<'_>) -> Result<Tick<Self::Out>>;
}
```

Nothing here says how state is stored, how inputs are fetched, or who calls
it. That is the whole trick. Because `cycle` is a free function over explicit
arguments, the *same* function can be driven by three execution strategies —
collectively **Nitro**, the tier system the `nitro!` macro emits:

| Strategy | Who owns state | Dispatch | Where |
|---|---|---|---|
| **Interpreted** | the `Builder`'s slots | one dyn call per node | `interp.rs` |
| **Compiled** | local variables in a generated fn | monomorphized, inlinable | `nitro!` |
| **Nested island** | locals inside one composite node | monomorphized inside, one dyn call outside | `nitro!` |

There is no duplicated cycle logic anywhere — no per-kind emitter strings, no
`cycle_inline` twins. **The strategies cannot drift, because there is only one
copy of the semantics.** If you find yourself writing a second implementation
of what a node does, stop: that is the invariant being broken.

### Why this matters more than it looks

The predecessor engine fused three concerns into one object — a node was its
computation *and* its storage (`RefCell` fields) *and* its input plumbing
(peeking upstream `Rc<dyn Stream>`s). A compiled backend then had no choice but
to **re-implement** every node's semantics as emitted source, which drifts the
moment anyone touches either copy. Types and closures were erased at wiring
time, so codegen reverse-engineered types from name strings and could never
recover a closure at all. And nothing declared "this node schedules
callbacks", so scheduling relied on name-based allowlists.

`Op` inverts all three: semantics are separable from storage, types and
closures survive into the generated code because they are ordinary generic
parameters, and `const ACTIVATION` states scheduling behaviour where a machine
can read it.

## The pieces

```
op.rs         The Op trait, Activation, Tick, Ctx — the vocabulary
interp.rs     The interpreted engine: slots, dirty list, dispatch, Runner
fluent.rs     GraphBuilder + Stream<T>; combinators as extension traits
ops.rs        The op catalog (map/filter/fold/join/delay/window, sources,
              and the statistics ops adapters/statistics.rs names)
latency.rs    Stamping (Stamping/StageSet), per-stage aggregation, the
              LatencyHandle a report reads out through
introspect.rs The wired topology as data + pictures (text/Mermaid/DOT/JSON/GML)
channel.rs    The Message envelope and senders — the thread boundary
async_source  produce_async: async producers, deterministic historical replay
adapters/     Optional, feature-gated, opt-in op surfaces. Mostly I/O — csv,
              kafka, zmq, kdb, redis, postgres, etcd, fix, web, ws, aeron,
              iceoryx2, fluvio, prometheus, otlp, lines — plus four that are
              not: statistics (EWMA + rolling windows), augurs (the same with
              a heavier kernel), market (the vocabulary venue adapters
              normalise into) and cache (a utility with no graph edge)
runtime/      Shared core: NanoTime, RunMode/RunFor, TimeQueue, Kernel,
              Burst, the latency data layer
signal.rs     A builder-less Signal facade over the fallible lifecycle
tier.rs       Tier: which nitro! engine runs a graph (see below)
```

Plus `wingfoil-derive` (`nitro!` and `#[op]`), `wingfoil-python`
(PyO3 bindings), `wingfoil-wire-types` + `wingfoil-wasm` + `js/` (the browser
side of the web adapter).

### `Tick<T>` — three outcomes, not two

```rust
enum Tick<T> { Value(T), Silent(T), Quiet }
```

`Value` updates the slot and ticks downstream. `Quiet` does neither. `Silent`
updates the value slot **without** ticking — which is exactly what `delay`
needs, and the reason a two-state "did it fire?" boolean is not enough.

### `Activation` — scheduling declared, not inferred

`NONE` (fires when an upstream ticks), `SCHEDULES` (also wakes itself via the
`TimeQueue` — tickers, delays), `THREADED` (fed from another thread through
the channel layer), `ALWAYS` (busy-spun every cycle — socket polling). It is a
`const`, so the interpreted engine reads it at wiring time and the compiled
emission folds it into a dispatch condition after monomorphization.

### `Tier` — develop interpreted, deploy compiled

A `nitro!` block emits both engines, but `interpreted()` (runner + handles you
read after running) and `compiled(run_mode, run_for)` (bounds in, values out)
are shaped differently, so a caller cannot swap them behind a flag. The macro
therefore also emits `run(tier, run_mode, run_for)`, which resolves to either
and returns the same output tuple. `Tier::default()` reads `WINGFOIL_TIER`,
falling back to the build profile — and since both engines are in the binary
regardless, that variable flips tiers **without a rebuild**.

The interpreted engine is the one to develop against: it honours the
`instrument-*` span features and supports dynamic graph surgery, neither of
which the monomorphized tiers have. Error context is *not* a difference — all
three name the failing node and hook, the monomorphized ones using the binding
name the user wrote (`node 5 (odd_str: map) cycle`, `island node 5 (…)`) where
the interpreted engine has only the op's `type_name` (`node 5 (Map) cycle`).
The labels are `&'static str`s baked in at expansion time, so they cost nothing
until something fails.

### Wiring is open, `Stream` is closed

Combinators are **extension traits** (`SourceOps`, `StreamOps`,
`StatisticsOps`, one per adapter), never inherent methods on `Stream<T>`. New
vocabulary is added through exactly two public primitives —
`GraphBuilder::source` and `Stream::wire` — so a downstream crate can add ops
that feel native without this crate knowing about them. `#[op(build = name)]`
generates the interpreted builder method *and* the `nitro!` forwarders from
the op's declared shape, so **built-in and user ops take the identical path**.
There is no per-op table in the macro. If you are editing a match arm to add
an op, you are on the wrong road.

## The rules that bite

These are the ones that cost someone real time. Each is written up where it
lives; this is the index.

**`TimeQueue` deduplicates by design.** Pushing a `(value, time)` pair already
queued is a no-op. A node scheduled twice for the same instant, or the same
feedback value sent twice in a cycle, must collapse to one event. It is
bounded on `PartialEq`, not `Hash + Eq`, specifically so `f64` payloads can
flow through `delay` and `feedback`. Do not "fix" either.

**Bursts, never latest-wins.** Same-instant values ride one `Burst<T>` and are
delivered atomically. A source that coalesces same-instant values is a bug —
the strictly-monotonic clock means a split same-time group cannot be
reassembled.

**No locks on the graph execution path.** `RefCell` for graph-thread-local
state; the channel layer to talk to background threads; `ArcSwap` where a
background thread needs an occasional read. A mutex in `cycle` is a
correctness problem, not just a performance one.

**A graph lives on one thread, and the types say so.** `GraphBuilder`,
`Stream<T>` and `Runner` hold `Rc`, so all three are `!Send`/`!Sync` — wiring,
`build()` and `run()` happen on the same thread, and moving any of them into
`thread::spawn` is a compile error. That is the *enforcement* of the rule
above, not a separate one. Everything that crosses a thread boundary crosses at
the channel layer, which hands out a `Send` half — `channel` /
`pooled_channel` / `source_at_start` return a sender to move; `spawn` and
`spawn_map` wire and run a whole sub-graph on a worker for you. Several
separate graphs on several threads is fine; sharing one is what is ruled out.
The rustdoc on `GraphBuilder` carries the table and a runnable example.

**I/O is established at `start()`, not at wiring.** Wiring stays pure — parse,
validate, reject the wrong run mode — so it is testable without a live
service, and connection errors surface during the run with node context.

**Live sources reject `RunMode::HistoricalFrom` at wiring.** A historical run
block-collects its whole input up front, so an unbounded live tail would
deadlock at `start`. Rejecting at wiring turns a hang into an error message.

**Production code does not `.unwrap()`.** `?` first, `.expect("invariant: WHY")`
where a precondition makes the branch unreachable. Mutex poisoning is
deliberately not recovered.

## Testing shape

Determinism comes from `RunMode::HistoricalFrom(NanoTime::ZERO)`: assert exact
values **and** exact tick times (`with_time()` + `accumulate()`). A test that
only checks values will not catch a scheduling regression.

Three tiers for anything with I/O:

1. `tests/<name>_adapter.rs` — no service required, runs in normal CI.
2. `tests/<name>_integration.rs` — needs a container or real sockets; compiled
   always, run in a per-adapter workflow.
3. `crates/wingfoil-python/tests/test_<name>.py` — the binding surface.

An op that reaches the compiled tiers should be asserted across all three
strategies in one test, comparing them against each other — that is how the
"cannot drift" property stays true rather than merely intended.

## Where to look next

| You want to… | Read |
|---|---|
| Add an op | `.claude/commands/new-op.md` |
| Add an I/O adapter | `.claude/commands/new-adapter.md` |
| Add Python bindings for one | `.claude/commands/bind-adapter.md` |
| Understand a deviation from the legacy engine | `docs/planning/deviation-register.md` |
| Know what is deferred and why | `docs/planning/port-plan.md` |
| Port code off the legacy engine | `docs/migration.md` |
