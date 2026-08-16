# Migrating Rust code from the legacy engine

Wingfoil's engine was rewritten. This page is the complete list of what
changes for Rust callers, and why. The Python half is
[`crates/wingfoil-python/docs/migration.rst`](../crates/wingfoil-python/docs/migration.rst)
— it stands on its own; this page does not repeat it.

> **Ruled 2026-08-03 (cutover-plan 1.4): there is no compatibility facade.**
> The new engine replaces the old one outright — the `MutableNode` wiring path
> retired with the legacy tree and nothing re-exports it under the new name.
> Rust downstreams break at the major version bump, deliberately, and this
> guide is the answer. The Python binding made the same call.

## The shape of the change

The old engine fused three concerns into one object. A node was its
computation *and* its storage (`RefCell` fields) *and* its input plumbing
(peeking upstream `Rc<dyn Stream>`s). The new engine separates them: an `Op`
says only what a node *computes*, and the engine owns the rest.

That is a real break, not a rename. In exchange, one definition of a node's
semantics now drives the interpreted engine, a fully-monomorphized compiled
runner, and compiled islands nested inside interpreted graphs — with no
duplicated cycle logic to drift. See
[`wingfoil-architecture.md`](wingfoil-architecture.md).

Everything the legacy tree could do, the new engine can do. The **one**
exception is listed under [What is gone](#what-is-gone).

## Writing a node: `#[node]` → `Op`

Before — state in fields, inputs peeked from stored upstreams:

```rust
#[node(active = [upstream], output = value: f64)]
impl MutableNode for ScaleStream {
    fn cycle(&mut self, _state: &mut GraphState) -> anyhow::Result<bool> {
        self.value = self.upstream.peek_value() * self.factor;
        Ok(true)
    }
}
```

After — config, state and inputs are parameters; the return says what to do:

```rust
pub struct Scale;

#[op(build = scale, fluent)]
impl Op for Scale {
    type Cfg = f64;              // construction-time config
    type State = ();             // engine-owned mutable state
    type In<'a> = (&'a f64,);    // typed inputs, passed in per cycle
    type Out = f64;
    const ACTIVATION: Activation = Activation::NONE;

    fn cycle(cfg: &mut f64, _state: &mut (), input: (&f64,), _ctx: &mut Ctx<'_>)
        -> Result<Tick<f64>>
    {
        Ok(Tick::Value(input.0 * *cfg))
    }
}
```

The mapping, item by item:

| Legacy | Now | Note |
|---|---|---|
| `&mut self` fields for scratch state | `type State` | Engine-owned; `Default`-seeded unless the op declares otherwise |
| `&mut self` fields for config | `type Cfg` | Closures live here |
| `self.upstream.peek_value()` | `input.0` | Typed and passed in; no stored upstream handles |
| `#[node(active = [a, b])]` | `type In<'a> = (&'a A, &'a B)` | Position is the edge order |
| `#[node(passive = [x])]` | `#[op(build = …, passive = [0])]` | A bitmask; positions index `In` |
| `Ok(true)` / `Ok(false)` | `Tick::Value(v)` / `Tick::Quiet` | **Plus `Tick::Silent(v)` — see below** |
| no declaration | `const ACTIVATION` | Scheduling is declared, not inferred from names |
| `fn setup/start/stop` | `fn start/stop/teardown` on `Op` | All return `anyhow::Result` |

### `Tick` has three states, not two

`Ok(true)`/`Ok(false)` could not express "update my value but do not tick
downstream" — the thing `delay` needs. That is `Tick::Silent(v)`. If you ported
a node mechanically to `Value`/`Quiet` and its downstream now fires when it
should not, `Silent` is what you want.

### Ops are generic; there is no per-op table

`#[op(build = name)]` generates the interpreted builder method **and** the
`nitro!` forwarders the compiled paths dispatch through, both derived from the
declared shape. Your op and a built-in op take the identical path. If you are
looking for a match arm to register a node in, there isn't one — that is the
design, not an omission.

An op generic over a type nothing in `Cfg`/`In`/`Out` mentions (a marker, a
unit, a latency stage) declares it `#[op(build = name, explicit = S)]` and is
called `.name::<S>()`.

## Wiring: the graph is explicit

Legacy sources were free functions returning `Rc<dyn Stream<T>>`, with the
graph assembled implicitly from whatever you passed to `run`. Now you hold a
`GraphBuilder` and build sources **on** it:

```rust
// before
let count = ticker(Duration::from_millis(10)).count();
count.run(RunMode::RealTime, RunFor::Cycles(100))?;

// after
let g = GraphBuilder::new();
let count = g.ticker(Duration::from_millis(10)).count();
let mut runner = g.build();
runner.run(RunMode::RealTime, RunFor::Cycles(100))?;
```

| Legacy | Now |
|---|---|
| `ticker(d)`, `constant(v)` — free fns | `g.ticker(d)`, `g.constant(v)` — on the builder |
| `Rc<dyn Stream<T>>` | `Stream<T>` (a cheap handle, `Clone`) |
| `node.peek_value()` | `runner.value(&handle)` |
| `node.run(mode, for)` | `g.build()` then `runner.run(mode, for)` |
| operator traits (`StreamOperators`) | extension traits (`StreamOps`, `SourceOps`) |

`RunMode`, `RunFor` and `NanoTime` are **unchanged** — literally the same
types, since both engines share one runtime core.

### Adapters: one trait method, not a free-fn/operator pair

Legacy exposed most adapters twice — a free function *and* an operator-trait
method. Now sinks are extension-trait methods on `Stream<T>` and sources are
free functions taking `&GraphBuilder` first:

```rust
use wingfoil::adapters::zmq::{ZeroMqPub, zmq_sub};

let (data, status) = zmq_sub::<Vec<u8>>(&g, RunMode::RealTime, "tcp://host:5556")?;
let sink = stream.zmq_pub(5556, ());
```

Adapters stay **out of the prelude** — opt in per adapter with
`use wingfoil::adapters::<name>::…;`.

**Statistics are an adapter, at the path legacy used.**
`wingfoil::adapters::statistics::StatisticsOps` is unchanged from 8.x, so a
`use` line pointing at it still resolves. What is new is the `statistics`
feature: legacy compiled the module unconditionally, and now, like every other
adapter, you ask for it.

```toml
wingfoil = { version = "9", features = ["statistics"] }
```

One consequence if you use statistics ops inside `nitro!`: the macro does not
glob feature-gated adapter traits into the module it generates, so the
surrounding file needs `use wingfoil::adapters::statistics::StatisticsOps;` —
the same import the fluent form needs.

Two behavioural differences worth knowing before you port an I/O graph:

- **Connections are established at `start()`, not at wiring.** Wiring is pure,
  so a connection error now surfaces during the run (with node context) rather
  than during graph construction.
- **Live sources reject `RunMode::HistoricalFrom` at wiring.** A historical run
  block-collects its input up front, so an unbounded live tail would deadlock
  at `start`. You get an error naming the bounded reader instead of a hang.

The full list of behavioural deltas, adapter by adapter, is
[`deviation-register.md`](planning/deviation-register.md).

## Errors

Every lifecycle function returns `anyhow::Result`. Propagate with `?` and add
`.context("…")` at I/O boundaries. A producer thread pushes an error into the
graph with `sender.send_error(e)`, which aborts the run with context.

Production code does not call `.unwrap()`; use `.expect("invariant: WHY")` only
where a precondition makes the branch unreachable.

## What is gone (and what replaced it)

**`Graph::export`** — the GML topology dump. The *name* is gone; the capability
is not, and is now strictly larger. The drop was deliberate (cutover-plan row
**2.1**, register **C6**) because we wanted a designed introspection story
rather than a same-shape port of a debug-only helper — that story is
[`introspect`](../crates/wingfoil/src/introspect.rs), and it has landed.

| legacy | wingfoil |
|---|---|
| `graph.export("g.gml")?` | `runner.snapshot().to_gml()` (or `g.snapshot()` before `build`) |

`GraphSnapshot` is a value rather than a side effect on a file path, so you can
assert on it in a test; it distinguishes active from passive edges, which GML
cannot express; and it renders to text, Mermaid, Graphviz DOT and JSON as well
as GML. See `examples/core/introspect/` and
[`docs/planning/introspection-plan.md`](planning/introspection-plan.md).

If you find anything else the legacy tree did that the new engine cannot, that
is a bug in the port, not an intended break — please report it.

## Latency capture

The stamping surface gains the [`Stamping`] mode and the fused `stamp_all`, one
signature moved, and the two `_if` stamps were **withdrawn** rather than kept —
the one place this surface is not a superset of legacy's, on record as
deviation **D28**. Every call they served is expressible as one `stamp_as`, and
the pair of them was how a stage got stamped twice.

| legacy | wingfoil |
|---|---|
| `s.stamp::<S>()` / `s.stamp_precise::<S>()` | unchanged |
| `s.stamp_if::<S>(on)` | **removed** — `s.stamp_as::<S>(Stamping::on_if(on))` |
| `s.stamp_precise_if::<S>(on)` | **removed** — `s.stamp_as::<S>(Stamping::new(on, true))` |
| — | `s.stamp_as::<S>(mode)` — the clock as a [`Stamping`] argument, so a config flag picks it in one call rather than two `_if`s with opposite polarities |
| — | `s.stamp_all::<(A, B)>(mode)` — several stages from one node, one payload clone instead of N |
| `let sink = s.latency_report(true);` | `let (sink, latency) = s.latency_report(ReportOutput::Stdout);` |

`ReportOutput` replaces the `print_on_teardown` bool (`false` →
`ReportOutput::Silent`), and adds `Log` — legacy wrote the summary to stdout
and nowhere else. The second element of the return is a `LatencyHandle`: it
reads out as labelled `HopStats` (`hops()`, `total()`), resets (`reset()` /
`take()`, without which one outlier pins a p99 for the life of the process),
and wires to a `Stream<LatencySnapshot>` with `windows(&g, period)` — at most
one per handle, because the windowed read is destructive: a second `windows`
stream would steal samples from the first, so wiring it panics. Branch the
returned stream to fan a window out to several consumers.

**One behavioural difference to know about, because it changes numbers you may
have been reading.** Legacy recorded a same-cycle hop — two stages sharing
`wall_time`'s per-cycle snap, so no measurement is possible — as a genuine 0 ns
sample, and silently dropped a backwards one. Both are now tallied instead
(`same_instant` / `backwards` / `unstamped`) and the report prints dashes and
the reason. A hop that legacy reported as `count 120, min 0, p50 0` will now
report `count 0` with a `120 same-cycle` note: the same underlying facts, minus
the claim that a measurement was taken. Stamp those stages with
`Stamping::Precise` to measure them for real.

## Interoperating during the transition

You do not have to move every process at once. The **ZeroMQ wire format is
byte-compatible between the two engines**, so a publisher on one can feed a
subscriber on the other, in either direction, including through the Python
bindings. That is covered by tests specifically so a staged rollout stays safe
(register **C2**).
