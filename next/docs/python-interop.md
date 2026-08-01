# Python interop — user-authored Rust components, composed and extended from Python

**Status:** the plugin-SDK layer is **built** — the object form, `#[pyop]`
(one to four inputs, stateless or stateful, tuple configs), `#[pygraph]` (any
arity, optional builder, including compiled islands), `#[pyadapter]` (source,
sink, burst, fallible, defaults), the edge conversions, and Python-defined
nodes in both the composition and subclass forms. What remains is breadth, not
mechanism: the per-adapter bindings (postgres, kafka, redis, etcd, fluvio,
csv, zmq, otlp, augurs, kdb, fix and prometheus done, three to go) and the Phase 4.5 mutable frontier for extending a *running*
graph.
**`wingfoil-next-python` is the go-forward Python binding: it supersedes the legacy `wingfoil-python`
bindings (decision 2026-07), it is not a new capability bolted beside a
preserved legacy surface.** The erased object form and `#[pyop]` seam below are
the foundation the legacy combinator / custom-stream / adapter surface is being
re-homed onto; at cutover next-python replaces `import wingfoil` (a breaking
change for Python users — see Phase 6 in `port-plan.md`). Earlier drafts framed
this as "distinct from keep-the-bindings-stable facade work"; there is no
separate legacy-facade track — this binding *is* the Python story.

## The goal

Let a user write, entirely in Rust:

- their own **IO adapters** (a Kafka-flavoured source, a bespoke socket sink…),
- their own **streams / ops** (custom `Op` implementations),
- their own **wiring logic** (a sub-graph: "these six nodes wired this way"),

compile them once, and then from Python:

- **use** the adapter,
- **reuse** the ops and the wiring,
- **extend** the Rust-authored wiring with more nodes — Rust *or* Python —
  and mix freely with wingfoil's built-in vocabulary.

In one line: **compiled-speed components authored in Rust, assembled and
extended dynamically from Python.** Legacy could never do this — it only let
Python compose the *built-in* node set, every value `PyElement`-boxed
end-to-end.

## Why next is the right substrate

The interpreted engine is *already* a dynamic, type-erased node list. Under the
typed `Handle<T>` façade (`interp.rs`):

```rust
slots: Vec<Rc<dyn Any>>,                       // erased value slots
// each node is a Box<dyn Fn(&mut Kernel) -> Result<bool>> pushed into a Vec
```

`Handle<T>` is a typed index into that erased list — the code calls it "the
erased, uniform counterpart to a typed `Op`". Two public seams already let
third-party code wire into it without touching the engine:

- **`GraphBuilder::register_op1` / `register_op2`** — wire an arbitrary
  `FnMut(&mut Cfg, &mut State, &A, &mut Ctx) -> Result<Tick<Out>>` closure in,
  with only `'static` bounds. `Stream::wire` dispatches through these.
- **Adapters are extension traits on `Stream<T>`** (`impl AugursForecastOps for
  Stream<f64>`) — a user adapter is a trait + impl in the user's crate, added
  with zero engine edits.

So the dynamic-composition substrate is done. What's missing is the *Python
boundary lane* over it.

## The one rule you cannot design away

**Rust generics monomorphize at compile time. New Rust code needs a Rust
compile step.** There is no JIT and no stable Rust ABI. A user who writes a new
op/adapter/wiring compiles it into an extension module — their crate depends on
`wingfoil-next`, built with `maturin`. What we will **not** build is a single
generic `wingfoil` wheel that ingests arbitrary user Rust ops at runtime via
`dlopen`/C-ABI — that path is type-erasure hell and breaks across compiler
versions.

This is the **proven, boring** model: it is exactly how **polars expression
plugins** work (user writes Rust, a macro generates the registration shim,
`maturin` builds the cdylib, Python composes the plugin with native polars).
Frame this feature as a **plugin SDK**, not a runtime-loader.

## Design decision 1 — one erased boundary type: `PyElement` in next

Everything Python-composable rides a single erased value type. Call it
`PyElement` (the legacy type already exists in `wingfoil-python`: a
`struct PyElement(Option<Py<PyAny>>)` implementing `Element` = `Debug + Clone +
Default + 'static`, plus `Add`/`Sub`/`Not`/`PartialEq` for the arithmetic ops).
Move an equivalent into a `wingfoil-next-python` crate.

The rule: **only the Python-exposed *edges* erase.** A node the user wants
Python to wire into is `Stream<PyElement>` / `Handle<PyElement>`. The
*interior* of a user's Rust op or sub-graph stays natively typed — it only has
to erase where a value crosses into Python-composable space. Conversions
(`PyElement: From<PyObject>` / `IntoPyObject`, and `TryInto<f64>` etc.) live at
those edges; legacy already carries them.

## Design decision 2 — a Python-held, still-open `GraphBuilder`

Python holds one shared builder object and keeps wiring into it; `.build()`
consumes it into a `Runner` exactly as in Rust. Handles surface to Python as an
erased object (`PyStream` wrapping `Handle<PyElement>` + a clone of the shared
builder). This is precisely the *"true `Rc<dyn Stream>` object form"* the port
plan already lists as remaining facade work — the interop feature needs it, so
it is the same deliverable, not a second one.

Extend-*before*-run needs only the builder staying open (easy). Extend a
*running* graph (add nodes after `run()` has started) needs the Phase 4.5
dirty-list / mutable-frontier engine — the plan already names 4.5 as the
enabler for dynamic graphs. Build-then-run covers the overwhelming majority of
uses, so this is not a v1 blocker.

## Design decision 3 — three registration macros

Mirror the existing `#[op]` macro, which already turns one `Op` impl into a
builder method plus the forwarders `graph!` dispatches through. Add a Python
emission target as a sibling. All three generate a PyO3 method that
monomorphizes the user's generic code at `PyElement` and registers it on the
Python-held builder.

### `#[pyop]` — expose a user `Op`

```rust
// user crate
pub struct ZScore<A>(PhantomData<A>);

#[op(build = zscore)]           // the normal interpreted/compiled/nested builder
#[pyop(name = "zscore")]        // + a PyStream.zscore(window) method at PyElement
impl<A> Op for ZScore<A>
where A: Into<f64> + Clone + 'static {
    type Cfg = usize;           // window
    type State = RollingStats;
    type In<'a> = (&'a A,);
    type Out = f64;
    const ACTIVATION: Activation = Activation::NONE;
    fn cycle(cfg: &mut usize, state: &mut RollingStats, input: (&A,), _ctx: &mut Ctx)
        -> Result<Tick<f64>> { /* ... */ }
}
```

**A user op becomes a free `#[pyfunction]`, not a `Stream` method.** pyo3 forbids
adding `#[pymethods]` to a *foreign* pyclass, so a user op in another crate
cannot become `stream.zscore(window)`. The feasible shape — the same one polars
expression plugins use — is a free function `module.zscore(stream, window)`.
`#[pyop]` (and, today, the `pyop!` declarative macro) emit that, wiring through
the public seam [`PyStream::wire_op1`], which erases at the edge:

```rust
// what `pyop!`/`#[pyop]` generate — a free function in the user's module:
#[pyfunction]
fn zscore(stream: PyRef<'_, Stream>, window: usize) -> Stream {
    Stream::from(stream.object().wire_op1::<f64, _, _, f64, _>(
        "zscore", Activation::NONE, window, RollingStats::default(),
        // op computes on f64; wire_op1 does f64 <- PyElement in, f64 -> PyElement out
        |cfg, state, a: &f64, ctx| ZScore::cycle(cfg, state, (&a,), ctx),
    ))
}
// Python:  z = wingfoil_next.zscore(stream, window=100)
```

**Status (shipped):** `PyStream::wire_op1` (the seam), the `pyop_fn!`
*declarative* macro, and the `#[pyop]` *proc* macro (reads an `Op` impl's
associated types + `cycle`, generates the `#[pyfunction]`) are implemented and
tested — Rust unit + cross-crate integration tests and pytest, including user
ops composed between built-in combinators, and both `scale` (`pyop_fn!`) and
`square` (`#[pyop]`) demos. `#[pyop]` v1 covers stateless single-input concrete
ops; stateful/multi-input shapes and `Cfg`-tuple arg naming call `wire_op1`
directly and are the remaining extensions.

### `#[pyadapter]` — expose a user adapter trait

An adapter is a source (threaded — `external`/`channel`-backed, already GIL-safe)
or an op-style transform. `#[pyadapter]` on the trait `impl` emits the matching
`PyGraphBuilder`/`PyStream` method, edge-converting at the boundary.

```rust
#[pyadapter(name = "my_socket_source", source)]
impl MySocketSourceOps for GraphBuilder {
    fn my_socket_source(&self, addr: &str) -> Stream<Trade> { /* ... */ }
}
// => PyGraphBuilder.my_socket_source(addr) -> PyStream   (Trade edge-erased to PyElement)
```

That trait form still works, but a **binding** should use the free-fn form: the
trait exists only to give the macro a receiver, and costs a throwaway trait plus
a second copy of every signature. Put the receiver first instead —
`&GraphBuilder` for a source, `&Stream<T>` for a sink — and give the Python name
separately (it must differ from the fn's own name, which the macro emits beside
it):

```rust
#[pyadapter(name = kafka_read, source)]
fn read(g: &GraphBuilder, brokers: String, topic: String)
    -> anyhow::Result<Stream<Burst<KafkaRecord>>> { /* ... */ }
// => wingfoil_next.kafka_read(graph, brokers, topic)
```

A **real** adapter needs two more things, both supported:

- **Fallible wiring.** Returning `Result<Stream<T>>` (any `…::Result`) makes the
  generated function return `PyResult`, so a wiring-time rejection — a bad run
  window, an unsupported run mode, a malformed config — raises a Python
  exception instead of aborting a later run.
- **Optional arguments.** A `#[pyo3(signature = (…))]` on the adapter method is
  forwarded to the generated `#[pyfunction]`, with the generated `graph`/`stream`
  receiver injected, so the author writes the signature over their own params
  only.

```rust
#[pyadapter(name = postgres_read, source)]
impl PostgresReadOps for GraphBuilder {
    #[pyo3(signature = (conn_str, query, time_col, start_nanos, duration_nanos,
                        chunk_secs = 3600, buffer_size = None))]
    fn postgres_read(&self, /* … */) -> anyhow::Result<Stream<Burst<PyPgRow>>> { /* … */ }
}
```

When the payload shape is only known from a *runtime argument* — a row written
from an arbitrary Python `dict`, whose columns the caller declares — the adapter
can stay on the erased type (`Stream<Burst<PyElement>>`) and marshal inside the
method, where that argument is in scope. `PyElement: TryFrom<&PyElement>` (the
identity conversion) is what lets such an adapter still go through the standard
`typed_burst_input` seam rather than a hand-written `#[pyfunction]`.

**What `#[pyadapter]` does not cover: a handle receiver.** The macro emits free
functions over a `&GraphBuilder` (source) or `&Stream<T>` (sink/transform)
receiver. A *stateful* server/exporter object — legacy's `WebServer`
(constructed, then `.port()` / `.sub(topic)`) and `PrometheusExporter`
(`.serve()` / `.register(name, stream)`) — has a lifecycle the free-fn form
cannot express, so those two bindings are hand-written as a `#[pyclass]` with
`#[pymethods]`, wiring through the same `PyGraph`/`PyStream` seams the macro
uses. Two adapters is not enough surface to justify a handle form in the macro;
revisit if a third appears.

**Erasing a burst attaches the GIL once, not once per element.** `PyGraph::run`
*detaches* for the duration of the run, so the graph thread does not hold the
GIL while cycling and every `Python::attach` inside a cycle is a real
`PyGILState_Ensure`/`Release` pair, contending with any other Python thread.
`T: Into<PyElement>` attaches per element (as do `PyElement::list` and `Clone`),
so an element-by-element erasure cost one full acquire per row — a thousand-row
burst paid a thousand of them per tick. Nested attaches short-circuit on a
thread-local count, so all three burst seams (`erase_burst_source`,
`erased_burst_output`, `typed_burst_input`) hoist a single attach around the
whole burst. A binding doing its own per-element Python work in a `map` /
`try_map` must do the same.

### `#[pygraph]` — expose user wiring logic

Write the wiring as a function over the shared builder; `#[pygraph]` exposes it
as a Python callable that **splices its nodes into the caller's builder** and
returns erased handles — so Python can wire onward from its outputs.

```rust
#[pygraph(name = vwap_pipeline)]
fn build_vwap(trades: &Stream<Trade>) -> Stream<f64> {
    // ... six-node vwap sub-graph, all at native `Trade`/`f64` ...
}
// => wingfoil_next.vwap_pipeline(trades) -> Stream, nodes spliced into the live builder
```

Any arity works: N `&Stream<T>` inputs, and a tuple return becomes a Python
tuple of streams. A leading `&GraphBuilder` — for wiring that creates nodes of
its own rather than only extending its inputs — makes the generated callable
take the graph first, `vwap_pipeline(graph, trades)`. With the builder and *no*
stream inputs, the sub-graph is a source. As with `#[pygraph]`'s other rule,
`name` must differ from the wiring fn's own name.

## Worked example — all three, composed and extended from Python

```python
import wingfoil_next as wf
from my_plugin import my_socket_source, vwap_pipeline, zscore   # user's Rust cdylib

g = wf.Graph()

# 1. user Rust IO adapter (source) — #[pyadapter(source)]
trades = my_socket_source(g, "tcp://feed:9000")

# 2. user Rust wiring logic, reused — #[pygraph], nodes spliced into g
vwap = vwap_pipeline(trades)

# 3. user Rust op, reused — #[pyop]
z = zscore(vwap, window=100)

# 4. EXTEND in Python: mix built-ins with a Python closure
signal = (z
          .filter(z.map(lambda v: abs(v) > 3.0))   # built-in filter + Python map
          .throttle(interval_nanos=1_000_000_000)  # built-in
          .distinct())                             # built-in

signal.print()
g.run(realtime=True)
```

Note the shapes pyo3 forces: a user op or adapter is a **free function**
(`zscore(vwap, …)`, not `vwap.zscore(…)`) because `#[pymethods]` cannot be added
to a foreign pyclass; and a source takes the graph explicitly, since Python has
no ambient graph.

Every node above — user adapter, user sub-graph, user op, built-ins, and a raw
Python lambda — is a peer in one erased interpreted graph. That is the whole
point: **the boundary is uniform, so composition is uniform.**

## The bonus: compiled islands under dynamic Python wiring

The Python lane lives on the **interpreted** engine, so the `compiled()` /
`nested()` LLVM-fusion path is off the table *for the Python-spliced portion* —
you cannot splice a Python node into a monomorphized graph. But a user can
author a hot sub-graph as a **`nested()` compiled island** and expose *that
island as a single erased node* Python wires around. **This is built**: an
island's `nested()` is `(&GraphBuilder, &Stream<In>…) -> Stream<Out>`, which is
exactly a builder-taking `#[pygraph]` wiring fn, so it needed no island-specific
macro — see `src/island.rs`:

```
compiled-speed interior (Rust, nested island)  ──►  dynamic wiring (Python)
```

So the envelope is strictly better than legacy: legacy was `PyElement` dynamic
dispatch on every node; here the expensive interiors run at compiled speed and
only the wiring seams are dynamic.

## What's missing (build list)

| Piece | Notes | Status |
|---|---|---|
| `PyElement` boundary type in a `wingfoil-next-python` crate | `Clone/Default/Debug/PartialEq` + `Add/Sub/Not` + scalar/`Py<PyAny>` edge conversions | ✅ done (`element.rs`) |
| Python-held open `GraphBuilder` + erased `PyStream` object | `PyGraph`/`PyStream` over the `Rc`-shared builder + runner slot | ✅ done (`graph.rs`) |
| `#[pyclass]` module (`Graph`/`Stream`) + maturin build + pytest | importable `wingfoil_next` module. **Mixed maturin layout**: the extension is the private `wingfoil_next._wingfoil` and the package under `python/` re-exports it (derived from `dir(_ext)`, not hand-listed) plus the pure-Python surface. `import wingfoil_next` is unchanged for callers | ✅ done (`python.rs`, `pyproject.toml`, `python/wingfoil_next/`) |
| Built-in combinator surface on `PyStream` (legacy `test_streams` parity) | `map`/`filter`/`merge`/`merge_all`/`delay`/`distinct`/`count`/`limit`/`throttle`/`sample`/`difference`/`not`/`inspect`/`accumulate`/`buffer`/`window`/`with_time`/`collect`/`fold`/`reduce`/`filter_map`/`filter_value`/`filter_none`/`sum`/`mean`/`average`/`bimap`/`split`/`dataframe` — all with Python-exception propagation; `Vec`→list & `(nanos,value)` tuple edge conversions | ✅ done (`graph.rs`, `python.rs`; PRs #549/#551 + follow-ups) |
| Python-defined custom node (legacy `CustomStream`) | Both forms. **Composition**: `Graph.custom_node(upstreams, obj)` over `GraphBuilder::custom_node` (the `MutableNode`+`StreamPeekRef` twin); `cycle(values)->bool` + `peek()` protocol. **Inheritance** (legacy's shape): `class MyStream(CustomStream)` with a no-arg `cycle()` reading `self.upstreams()`, constructor returning the wired `Stream` — a pure-Python base class in `python/wingfoil_next/stream.py` layered on the composition form. Single-run either way | ✅ done (`graph.rs`, PR #549; `stream.py`) |
| `pyop_fn!` seam + declarative macro | `PyStream::wire_op1` + `pyop_fn!` for stateless single-input ops | ✅ done (`macros.rs`) |
| `#[pyop]` **proc** macro | reads an `Op` impl → `#[pyfunction]`; v1 stateless single-input concrete | ✅ done (`wingfoil-next-python-macros`) |
| `#[pyop]` extensions | **done** — `State` is any `Default`-seedable type (re-seeded per run); `In<'a> = (&A, &B)` emits `module.name(stream, other)` over `wire_op2`, `(&A, &B, &C)` emits `module.name(stream, second, third)` over `wire_op3` / `Builder::register_op3` (the general form of the `Join3`-specialised `trimap`), and `(&A, …, &D)` over `wire_op4` / `register_op4`; stream parameters are named, so they take keywords. A tuple `Cfg` destructures into one named Python parameter per element via `arg = (p1, p2, …)`. The emitter is arity-generic — arity 5+ needs only a `register_op<n>`/`wire_op<n>` pair plus a parameter name. Note this ceiling is on *Rust-authored* ops (heterogeneous static input types, no variadic generics); a Python-defined node via `Graph.custom_node` takes any number of erased upstreams | ✅ done |
| `#[pygraph]` macro (wiring reuse) | expose a Rust-authored sub-graph as a Python callable, spliced into the caller's graph; interior runs at native types, only the edges erase. **Any arity**: N `&Stream<T>` inputs, and a single `Stream<U>` or a tuple of them out (Python gets a tuple of streams). An optional leading `&GraphBuilder` — for wiring that creates nodes of its own — makes the generated fn take the graph first; builder-only (no stream inputs) is a *source* sub-graph, erasing via `erase_source`. Over the `typed_input`/`erased_output` seams | ✅ done |
| `#[pyadapter]` macro (source + sink + burst) | **source, sink/transform, and burst adapters done** — `#[pyadapter(name = …, source)]` on `impl Trait for GraphBuilder { … -> Stream<T> }` emits `module.m(graph, …)`; `#[pyadapter(name = …)]` (no marker) on `impl Trait for Stream<T> { … -> Stream<U> }` emits `module.m(stream, …)`. `Stream<Burst<T>>` erases to a Python `list` per tick, and on the way in a Python `list`/`tuple` rebuilds a multi-value burst (else a single-element burst) — so a burst source round-trips into a burst sink. Over the `builder`/`erase_source`/`erase_burst_source` and `typed_input`/`typed_burst_input`/`erased_output`/`erased_burst_output` seams; a sink's `Stream<()>` erases to `None`. Adapter method params must be `FromPyObject`. **Fallible wiring** (`Result<Stream<T>>` → a `PyResult` fn, wiring errors raised as Python exceptions) and **forwarded `#[pyo3(signature = …)]`** (Python defaults for optional args, with the generated receiver injected), and the **free-fn form** (receiver as the first param — no throwaway trait, no duplicated signature; the impl form remains for adapters wanting a fluent Rust trait) landed with the first real adapter binding | ✅ done |
| Per-adapter Python bindings (`crate::adapters::*`) | The `#[pyadapter]` exposure of the real `wingfoil_next::adapters::*` I/O adapters, each behind a cargo feature of the same name and registered in the `#[pymodule]` under the same `#[cfg]`. **postgres + kafka + redis + etcd + fluvio + csv + zmq + otlp + augurs + kdb + fix + prometheus done**: `postgres_read` / `postgres_sub` / `postgres_source` / `postgres_write` / `postgres_notify_trigger_sql`, with a dynamic row↔`dict` edge (`PyPgRow`) and declared-column write marshaling; unit-level marshaling tests plus a service-backed pytest leg in `postgres-next-integration.yml`. **kafka**: `kafka_sub` / `kafka_pub`, an event↔`dict` edge plus dict-to-`KafkaRecord` write marshaling with a `topic` fallback. **redis**: `redis_sub` / `redis_pub` / `redis_stream_read` / `redis_stream_write`, with the `RecordDict` sink-marshaling reader now factored into `adapters::common`. **etcd**: `etcd_sub` / `etcd_pub`, a watch-event↔`dict` edge with a string `kind`, plus str-or-list endpoints for a cluster. **fluvio**: `fluvio_sub` / `fluvio_pub`. **csv**: `csv_read` / `csv_write` over a positional `Vec<String>` with the column names supplied by the binding — which needed a new `CsvSinkOps::csv_write_with_header` on the *engine* adapter, since the serde-derived header is empty for a positional record. **zmq**: `zmq_sub` / `zmq_pub` plus their etcd-discovery twins — the binding that made `#[pyadapter]` accept a **tuple return**, so a `(data, status)` source hands Python a tuple of streams (mirroring `#[pygraph]`). **otlp**: `otlp_push`, which relaxed the engine's `&'static str` metric name to `impl Into<Cow<'static, str>>` and so removed the `Box::leak` legacy did per wiring call. **augurs**: the six analytics fns, each yielding the *full* result dict rather than legacy's headline number. **kdb**: `kdb_read` / `kdb_sub` / `kdb_write`, a second dynamic row↔`dict` edge (`PyKdbRow`, dispatched on each value's actual q type) plus declared-column write marshaling; `kdb_sub` is new capability legacy never bound. **fix**: `fix_connect` / `fix_accept` / `fix_send` plus the hand-written `fix_connect_tls` returning a `FixConnection` handle class — the first *mixed* binding, and the rehearsal for the handle-pyclass tier below. **prometheus**: the `PrometheusExporter` handle class (`serve` + `gauge`), the first of that tier, with its whole test tier running by default — the exporter is the server, so a test scrapes its own `/metrics` over loopback. The recipe is the `/bind-adapter-next` skill. **Remaining: 3** — legacy `wingfoil-python` binds 15 adapters in all, in four tiers: *mechanical* (all ✓: kafka, redis, etcd, fluvio, csv, zmq — a scalar/bytes payload and the free-fn form); *dynamic payload* (all ✓: kdb, fix — a `PyPgRow`-shaped stand-in plus column marshaling); *handle pyclass* (prometheus's `PrometheusExporter` ✓, web's `WebServer` — **not** a shape `#[pyadapter]` can generate, see below); and *stream transform* (all ✓: augurs' six fns, otlp's `otlp_push` — legacy exposes these as `stream.method(…)`, next as free fns) | 🟡 12 done, 3 to go |
| Compiled graph reachable from Python | **done, and better than the original POC** — rather than one hard-coded `compiled()` graph, a `graph!`-generated **`nested()` island** is exposed through `#[pygraph]`: its signature is `(&GraphBuilder, &Stream<In>…) -> Stream<Out>`, i.e. exactly a builder-taking `#[pygraph]` wiring fn, so no island-specific machinery was needed. The interior is monomorphized straight-line code; Python wires around it dynamically. A pytest asserts the island's values *and* tick times match its interpreted twin, and that it composes with Python `map` on both sides. Still true: `compiled()`/`nested()` are not Python-*splittable* — an island is one opaque node | ✅ done |
| Edge-conversion trait bounds | **done** — every integer width (`i8`…`isize`, `u8`…`usize`), `f32`/`f64`, `bool`, `String`/`&str`, `()`→`None`, `Vec<u8>`→**`bytes`** (not a list of ints), and `Option<T>`→`None`/value with the inner conversion propagating a wrong-typed value as an error rather than a silent `None`. A user record type crosses via its own `From`/`TryFrom` impls — legal from a downstream crate under the orphan rules, proven by a `Trade` struct defined in the external seam-test crate | ✅ done |
| Mutable-frontier engine (extend a *running* graph) | Phase 4.5 dirty-list; only needed for post-`run` mutation | 🟡 Phase 4.5 |

## Constraints / non-goals

- **New Rust ⇒ a compile step.** No runtime `dlopen` of arbitrary Rust ops.
  Plugins are `maturin`-built cdylibs (polars-plugin model).
- **Only edges erase.** Interior types stay native; do not `PyElement`-box the
  whole interior (that was the legacy tax).
- **`compiled()`/`nested()` are not Python-splittable** — expose them as
  single island nodes instead.
- Boundary types must be `Element` (`Debug+Clone+Default+'static`) and
  edge-convertible.
- Threaded adapters across the GIL already work (`external`/`channel` are
  `THREADED` sources; the runner detaches the GIL during `run`, as legacy does).

## Suggested sequencing

1. Land the erased-boundary `PyElement` + the open object-form `GraphBuilder`
   in next (this is also the port plan's remaining facade item — do it once).
2. `#[pyop]` first (smallest, highest-leverage; unlocks user ops).
3. `#[pygraph]` (wiring reuse — the "extend in Python" headline).
4. `#[pyadapter]` (sources/sinks; leans on the threaded-source plumbing).
5. ✅ the compiled-island form — which fell out of giving `#[pygraph]` a
   `&GraphBuilder` parameter, rather than needing a `nested` marker of its own.

One PR per macro; each carries a round-trip test (author in Rust → compose and
extend in Python → assert values + tick times against the same graph authored
purely in Rust, the parity-oracle discipline used everywhere else in next).
