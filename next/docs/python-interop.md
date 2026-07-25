# Python interop — user-authored Rust components, composed and extended from Python

**Status:** design sketch. Post-Phase-6 / cutover-era capability. Distinct from
the Phase 6 "keep the existing bindings stable" facade work (that keeps the
*current* PyElement-authored surface running on the new engine; this adds a
*new* capability on top).

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

`#[pyop]` emits (sketch):

```rust
#[pymethods]
impl PyStream {
    #[pyo3(name = "zscore")]
    fn py_zscore(&self, window: usize) -> PyResult<PyStream> {
        // monomorphized at PyElement; edge-converts PyElement -> f64 in, f64 -> PyElement out
        let out = self.builder.borrow_mut().register_op1(
            self.handle, "zscore", Activation::NONE,
            window, RollingStats::default(),
            |w, s, a: &PyElement, ctx| {
                let x: f64 = a.try_into()?;
                Ok(ZScore::cycle(w, s, (&x,), ctx)?.map(PyElement::from))
            });
        Ok(self.wrap(out))
    }
}
```

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

### `#[pygraph]` — expose user wiring logic

Write the wiring as a function over the shared builder; `#[pygraph]` exposes it
as a Python callable that **splices its nodes into the caller's builder** and
returns erased handles — so Python can wire onward from its outputs.

```rust
#[pygraph(name = "vwap_pipeline")]
pub fn vwap_pipeline(g: &mut GraphBuilder, trades: Handle<Trade>) -> Handle<f64> {
    let px  = g.register_op1(trades, /* price */ ..);
    let vol = g.register_op1(trades, /* size  */ ..);
    // ... six-node vwap sub-graph ...
    vwap
}
// => wingfoil.vwap_pipeline(trades: PyStream) -> PyStream, nodes spliced into the live builder
```

## Worked example — all three, composed and extended from Python

```python
import wingfoil as wf
from my_plugin import my_socket_source, vwap_pipeline   # user's Rust cdylib

g = wf.GraphBuilder()

# 1. user Rust IO adapter (source)
trades = g.my_socket_source("tcp://feed:9000")

# 2. user Rust wiring logic, reused — nodes spliced into g
vwap = vwap_pipeline(trades)

# 3. user Rust op, reused
z = vwap.zscore(window=100)

# 4. EXTEND in Python: mix built-ins + a Python closure + another user op
signal = (z
          .filter(z.map(lambda v: abs(v) > 3.0))   # built-in filter + Python map
          .throttle("1s")                          # built-in
          .distinct())                             # built-in

signal.print()
g.run(realtime=True)
```

Every node above — user adapter, user sub-graph, user op, built-ins, and a raw
Python lambda — is a peer in one erased interpreted graph. That is the whole
point: **the boundary is uniform, so composition is uniform.**

## The bonus: compiled islands under dynamic Python wiring

The Python lane lives on the **interpreted** engine, so the `compiled()` /
`nested()` LLVM-fusion path is off the table *for the Python-spliced portion* —
you cannot splice a Python node into a monomorphized graph. But a user can
author a hot sub-graph as a **`nested()` compiled island** and expose *that
island as a single erased node* Python wires around:

```
compiled-speed interior (Rust, nested island)  ──►  dynamic wiring (Python)
```

So the envelope is strictly better than legacy: legacy was `PyElement` dynamic
dispatch on every node; here the expensive interiors run at compiled speed and
only the wiring seams are dynamic. `#[pygraph(nested)]` would emit the island
form.

## What's missing (build list)

| Piece | Notes | Status |
|---|---|---|
| `PyElement` boundary type in a `wingfoil-next-python` crate | move/adapt the legacy type; `Element` + `Add/Sub/Not/PartialEq` + `From<PyObject>`/`IntoPyObject` | legacy proves it |
| Python-held open `GraphBuilder` + erased `PyStream` object | **same deliverable** as the port plan's "true `Rc<dyn Stream>` object form" | 🟡 flagged in plan |
| `#[pyop]` / `#[pyadapter]` / `#[pygraph]` macros | fourth emission target beside `#[op]`'s interpreted/compiled/nested | new |
| Edge-conversion trait bounds | `PyElement <-> f64/Trade/…` at the seams only | legacy proves it |
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
5. `#[pygraph(nested)]` compiled-island form once the above is proven.

One PR per macro; each carries a round-trip test (author in Rust → compose and
extend in Python → assert values + tick times against the same graph authored
purely in Rust, the parity-oracle discipline used everywhere else in next).
