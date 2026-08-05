Add the **Python bindings** for the already-ported wingfoil I/O adapter
named `$ARGUMENTS`, in `crates/wingfoil-python/src/adapters/`.

This is the *binding* task, not the port. It assumes
`crates/wingfoil/src/adapters/$ARGUMENTS*` already exists and passes
its own tests — if it does not, run `/new-adapter $ARGUMENTS` first and
come back. (`/new-adapter` links here for its Python step, so a brand-new
adapter ends up running both.)

`wingfoil-python` is the **go-forward** Python binding: it supersedes the
legacy `wingfoil-python`, it is not a facade over it (decision 2026-07, see
`docs/python-interop.md` and Phase 6 of `docs/port-plan.md`). At
cutover the `wingfoil` Python module name passes from the legacy bindings to
these.

## Read first

- **`crates/wingfoil-python/src/adapters/postgres.rs`** — the first
  real adapter bound and the template for every one after it. Read it before
  writing code; most of what follows is a generalisation of what it does.
- `crates/wingfoil-python/src/adapters/common.rs` — the shared
  run-shape helpers.
- `crates/wingfoil-python/src/python.rs` — `ramp_source` /
  `list_sink` / `pair_source` / `burst_list_sink`, the four minimal
  `#[pyadapter]` demos (plain + burst, source + sink).
- `crates/wingfoil-python/tests/plugin_seam.rs` — the same seams
  exercised from an *external* crate.
- `docs/python-interop.md` — why the boundary is shaped this way.

## The parity obligation

Legacy `legacy/wingfoil-python/src/py_$ARGUMENTS.rs` is your **parity oracle**, the
same way the legacy adapter is for the Rust port. Before writing anything,
inventory it:

- every `#[pyfunction]`, `#[pyclass]`, and `*_inner` helper it exposes;
- every argument, including defaults and the types they accept;
- the pytest cases in `legacy/wingfoil-python/tests/` that cover it.

Every one needs a wingfoil equivalent **or** an explicit deviation note in the
binding module's `//!` header. Do not silently drop a legacy entry point. Where
wingfoil's Rust adapter has capability legacy never had — a unified
`$ARGUMENTS_source`, a `buffer_size` bound — expose it too and say so in the
docs; the superset objective runs through the bindings as well.

Cross-cutting legacy↔wingfoil differences go in `docs/deviation-register.md`.

## Feed lessons back into this skill

Like `/new-adapter`, this file is a **living document**. The first binding
(postgres) grew three capabilities in the `#[pyadapter]` macro itself and the
whole of `adapters/common.rs`. When a binding surfaces something not captured
here — a repeated conversion, a boundary pitfall, a CI gate — fold it back in,
ideally in the same PR.

## 1. Branch

Cut from `next`, never `main` (see `CLAUDE.md`):

```bash
git checkout next && git pull origin next && git checkout -b bind-$ARGUMENTS-python
```

The PR targets base `next`.

## 2. Feature gate — `crates/wingfoil-python/Cargo.toml`

**First check whether the thing you are binding is feature-gated at all.** Not
every Python surface is an adapter: `wingfoil::latency` is an
unconditional engine module, so its binding (`src/latency.rs`, *not* under
`adapters/`) has no cargo feature, is registered in the `#[pymodule]`
unconditionally, ships in every wheel, and needs no integration workflow. If
`grep '^pub mod <name>' crates/wingfoil/src/lib.rs` shows no
`#[cfg(feature = …)]` above it, skip this whole step and step 3's `#[cfg]`s —
gating a binding whose engine side is always compiled only creates a way to
build a wheel that is missing it for no reason. Everything else in this file
still applies.

Each *adapter* binding lives behind a `wingfoil-python` cargo feature of
the **same name** as the adapter, which turns on the matching engine feature:

```toml
$ARGUMENTS = ["wingfoil/$ARGUMENTS", "_common"]  # + any dep: it names directly
all-adapters = ["postgres", "$ARGUMENTS", ...]
```

`_common` is internal: `adapters::common` is compiled for *any* adapter and
names `async_source::RunParams`, so every adapter feature must reach
`wingfoil/async` through it. Leaving it off still builds under
`all-adapters` — some other adapter supplies `async` — and fails only for
someone building yours alone. Verify with
`cargo check --manifest-path crates/wingfoil-python/Cargo.toml --features $ARGUMENTS`.

Add a comment saying what the feature exposes, as the `postgres` entry does.

Name a dependency directly (`dep:chrono`) only if your *binding* code names its
types — postgres does, because the row decoder mentions `NaiveDateTime`.
Transitive availability through the engine feature is not enough.

Two roll-ups to keep straight:

- **`all-adapters`** is what `python-test.yml` builds
  (`cargo test --manifest-path crates/wingfoil-python/Cargo.toml --features all-adapters`), and that job
  installs only `protobuf-compiler` and `patchelf`. An adapter needing a system
  library at build time (clang, CMake, a vendored C lib) **must not** join
  `all-adapters` without also adding the install step to that workflow. Say
  which you did in the PR.
- **`[tool.maturin] features` in `pyproject.toml`** is what ships in the default
  wheel. A published wheel is the only copy a user gets, so ship everything you
  can; the criterion for leaving one out is **portability**, not build time. Features are listed one by one there on purpose — adding one is a
  considered decision. Pure-Rust adapters can ship by default; anything needing
  a system library stays opt-in (`maturin develop -F $ARGUMENTS`) or it breaks
  the wheel build for everyone.

`aeron` is the worked example of an adapter that stays out of both: it builds
the Aeron C library from source, so it is opt-in via `maturin develop -F aeron`
and tested only in its own workflow. Two consequences to plan for:

- your Rust `#[cfg(test)]` tests do **not** run in `python-test.yml` — the
  adapter workflow's Python leg is their only home, so make that leg run them
  (or say plainly in the PR that they do not run there);
- **`maturin develop -F x` REPLACES the `pyproject.toml` feature list, it does
  not add to it.** So an opt-in leg must spell out `-F extension-module,x`, and
  the feature must be **self-sufficient on its own**. Check it:
  `cargo check --manifest-path crates/wingfoil-python/Cargo.toml --features <name>` — the `all-adapters`
  roll-up hides a missing implication, because some other adapter supplies it.
  `adapters::common` names `async_source::RunParams`, so every adapter feature
  has to reach `wingfoil/async` — that is what the internal `_common`
  feature is for, and your new feature must list it;
- a dynamically-linked native library (aeron's `libaeron.so`) lives in the
  cargo build directory and is not on the loader path, so the pytest step needs
  `LD_LIBRARY_PATH` pointed at it or the extension fails to *import*.

## 3. Module registration

`src/adapters/mod.rs` — gate the module and note it in the header:

```rust
#[cfg(feature = "$ARGUMENTS")]
pub mod $ARGUMENTS;
```

`common` is gated on *any* adapter being on, so extend its `#[cfg]` when you add
the first adapter after postgres:

```rust
#[cfg(any(feature = "postgres", feature = "$ARGUMENTS"))]
pub mod common;
```

`src/python.rs` — register the generated `#[pyfunction]`s in
`register_adapters` under the **same** `#[cfg]`, importing them **by name**:

```rust
#[cfg(feature = "$ARGUMENTS")]
{
    use crate::adapters::$ARGUMENTS::{$ARGUMENTS_read, $ARGUMENTS_write};
    m.add_function(wrap_pyfunction!($ARGUMENTS_read, m)?)?;
    m.add_function(wrap_pyfunction!($ARGUMENTS_write, m)?)?;
}
```

`wrap_pyfunction!` needs pyo3's hidden wrapper in scope, so a module-qualified
path (`crate::adapters::foo::bar`) does **not** resolve. Import by name.

## 4. Write the bindings — `src/adapters/$ARGUMENTS.rs`

(Or `src/$ARGUMENTS.rs`, for a non-adapter engine module — see step 2.)

### Use the free-fn form, receiver first

```rust
#[pyadapter(name = $ARGUMENTS_read, source)]
fn read(g: &GraphBuilder, addr: String) -> Result<Stream<Burst<Record>>> { … }
//  => wingfoil.$ARGUMENTS_read(graph, addr) -> Stream

#[pyadapter(name = $ARGUMENTS_write)]
fn write(stream: &Stream<Burst<Record>>, addr: String) -> Result<Stream<()>> { … }
//  => wingfoil.$ARGUMENTS_write(stream, addr)
```

- `source` marker → receiver is `&GraphBuilder`; no marker → receiver is
  `&Stream<T>` (sink or transform). A sink's `Stream<()>` erases to `None`.
- `name` **must differ** from the fn's own name — the macro emits a
  `#[pyfunction]` of that name beside it. Hence the short `read` / `write` /
  `sub` / `source`; the module is already `$ARGUMENTS`.
- The `impl Trait for GraphBuilder` form still works but **prefer the free fn**:
  the trait exists only to give the macro a receiver, and costs a throwaway
  trait plus a second copy of every signature. A binding never calls itself
  fluently from Rust.
- Params become `#[pyfunction]` params, so they must be `FromPyObject`. A
  Rust-only handle (`Rc<RefCell<…>>`) cannot cross.
- Values edge-convert: `T: TryFrom<&PyElement>` in, `U: Into<PyElement>` out.

### Bindings are fallible

Real adapters validate at wiring — run window, run mode, config. Return
`Result<Stream<T>>` and `#[pyadapter]` generates a `PyResult` fn, so the
rejection raises a Python exception instead of aborting a later run.

### Optional arguments

Put `#[pyo3(signature = (…))]` on the adapter fn over **your own** params; the
macro forwards it and injects the `graph`/`stream` receiver.

### The `///` on the adapter fn *is* the published Python docstring

`#[pyadapter]` (and `#[pyop]` / `#[pygraph]`) copy the annotated item's doc
comment onto the generated `#[pyfunction]`, so it becomes what `help()` prints
**and** the function's entry in the generated Sphinx reference
(`crates/wingfoil-python/docs/`, "Generated reference"). There is nowhere
else that prose gets written — `api.rst` carries a hand-written index, not
per-function text.

So write it for the **Python** caller, not a Rust reader: what one tick
carries (`list` of dicts? `bytes`? a `(data, status)` tuple?), what every
argument means, which selectors are strings and what the accepted set is, and
what raises at wiring versus what aborts the run. On the impl form of
`#[pyadapter]` the doc goes on the **method**, not the trait or the `impl`
block. Check it landed:

```python
import wingfoil as wf; help(wf.my_adapter_sub)
```

### Run mode / run window become arguments

A Python `Graph` does not know its run mode until `run()`, so a source that
needs the window (to slice queries) or the mode (to reject the wrong one) at
*wiring* takes it explicitly — `start_nanos` / `duration_nanos` / `realtime` —
and the docstring must say they have to match the eventual `graph.run(...)`.

`crate::adapters::common` already carries the conversions:
`historical_params(start_nanos, duration_nanos)`, `realtime_params()`,
`run_mode(realtime)`, `secs_to_nanotime(secs)`. Use them; add to that module
when you find the next repeated one.

**Expose the unified `$ARGUMENTS_source` where the Rust adapter has one.** It is
the mode-agnostic wiring, and Python — where the run mode is an argument anyway
— is exactly where that ergonomic pays off.

### Bursts

Most real adapters are burst-shaped. `Stream<Burst<T>>` erases to a Python
**`list`** per tick (the same-instant group, losslessly); on the way *in* a
Python `list`/`tuple` rebuilds a multi-value burst, anything else a
single-element burst. So a burst source round-trips into a burst sink.

Do **not** collapse a burst to its last value to get a scalar-per-tick shape.
Legacy `py_postgres_read` did, and silently dropped rows sharing a timestamp;
wingfoil's read is lossless and the caller writes `[0]` for the single-row case.

### Dynamic payloads

Where a Rust caller writes a record struct, Python has none — supply a dynamic
stand-in:

- **Reads** decode into a plain-Rust intermediate implementing
  `From<T> for PyElement`. It must contain **no `Py<PyAny>`**: rows are decoded
  on the adapter's worker thread and cross a channel, so the intermediate has to
  be `Send` Rust data, with the Python object built later on the graph thread.
  See `PyPgValue` / `PyPgRow`.
- **Writes** usually need a runtime `columns`-style argument to interpret the
  Python value, and that argument is not in scope at the `typed_burst_input`
  seam. Keep the sink's input **erased** (`&Stream<Burst<PyElement>>`, via the
  identity `PyElement: TryFrom<&PyElement>`) and marshal inside the fn with a
  `try_map`.

A third case is an argument that changes the **value shape itself**, not just
how a fixed one is interpreted — iceoryx2's `stages`, which turns each sample
from `bytes` into a `TracedBytes`. A `#[pyadapter]` fn has one return type, so
the branch cannot live at the erasure seam: return `Stream<Burst<PyElement>>`
and do the decode in a `map`/`try_map` of your own (one `Python::attach` around
the whole burst), leaving the seam to erase already-Python values to a `list`.
The sink direction is the erased-input rule above. Say in the module header
that the extra node is deliberate — it is the price of one signature covering
both shapes, and it costs a `Vec` per tick, not a GIL acquire.

Two payload shapes show up. Postgres declares its schema once and decodes
against it; kdb tags **every value** with its own type, so `PyKdbRow` dispatches
on `k.get_type()` per column. Either way the *unsupported* arm is an error (see
below), never a `format!("{v:?}")` fallback — a debug string in a dict reads as
a plausible value.

**Name the wire crate's types through the engine's re-export**, not by adding a
dependency. If the adapter module does not already re-export what the decoder
needs, add a `pub use` there (kdb's `qtype` constants landed in
`adapters::kdb` for exactly this) — that keeps the binding pinned to whatever
version the engine builds against, which is the only version that can be
correct.

**Marshaling fails loudly.** A missing key, an unsupported declared type, a
wrong-typed value, or an unsupported column type from the wire aborts the run
with a message naming the field and listing what *is* supported. Never a silent
`NULL`, `None`, or empty string. (Legacy prometheus/otlp do
`elem.str().unwrap_or_default()` — a failed conversion becomes an empty string.
Do not port that; note it as a deviation.)

**Validate every argument before contacting the service.** A binding that
resolves a connection at wiring (aeron through the media driver, kdb's
credentials) must run its own checks first — otherwise a caller's typo in
`mode=` or `realtime=` reports itself as a *driver timeout*, and costs a
connect attempt to say so. Where the engine's own check comes too late, repeat
it in the binding rather than accept the worse message.

**Prefer string arguments to `#[pyclass]` enums** for mode/type selectors, with
a loud error listing the accepted values — the convention postgres set for SQL
column types. Legacy uses enum pyclasses for `AeronMode` /
`Iceoryx2ServiceVariant` / `Iceoryx2Mode`; a string keeps the module surface
small and needs no extra class registration. Note the deviation.

### Shapes `#[pyadapter]` does not cover

The macro emits free functions over a `&GraphBuilder` or `&Stream<T>` receiver.
It has **no handle-receiver form**, so a stateful server/exporter object —
legacy's `WebServer` (constructed, `.port()`, `.sub(topic)`) and
`PrometheusExporter` (`.serve()`, `.register(name, stream)`) — is not a shape it
can generate. Hand-write those as a `#[pyclass]` with `#[pymethods]`, wiring
through the same `PyGraph`/`PyStream` seams the macro uses
(`builder()`, `erase_source`, `typed_input`, …). Flag it in the PR rather than
bending the adapter into a free fn that loses the lifecycle.

`fix` is the worked example, and shows the two rules that matter:

- **A binding can be a mix.** `fix_connect` / `fix_accept` / `fix_send` are
  `#[pyadapter]`; only `fix_connect_tls` (which returns the engine's
  `FixConnection` handle) is hand-written. Do not hand-write the whole module
  because one entry point needs to be.
- **The hand-written fn erases at the same seams.** Take
  `graph: PyRef<'_, Graph>`, call `graph.object()`, wire on `.builder()`, and
  erase each output with `erase_burst_source::<T>` / `erase_source::<T>` —
  exactly what the macro's source arm emits. Store the resulting `PyStream`s in
  the `#[pyclass]` (`PyStream` is `Clone`) and hand them out through `#[getter]`s
  as `Stream::from(…)`. Register the class with `m.add_class::<…>()` beside the
  functions, under the same `#[cfg]`.

`prometheus` is the minimal one: `PrometheusExporter` with `serve()` and
`gauge(name, stream)`. Note what it does *not* need — the exporter owns no graph
state, so it takes no `Graph` at all; the stream it is handed carries its own.
Take a `Graph` only when the handle actually wires a source, as web's
`server.sub(graph, topic)` does.

A handle's own methods are the *only* place marshaling runs synchronously
(`FixConnection.send` converts before the message reaches the session thread),
which makes them a free unit harness for the dict→record path — no run, no
service. Reach for that when a sink's own errors are unreachable because the
adapter connects at `start()` before the first cycle.

The other gap is **an op whose config is a runtime value the typed Rust op
takes as a type parameter**. `latency` is the worked example: a Rust stamp
names its stage as a `Stage` *type*, so there is no `Op` impl the binding can
wire — it registers its own step through `PyStream::wire_op1` with the stage
name in `Cfg`. Two consequences worth knowing before you start:

- **`wire_op1` has no lifecycle hooks**, and `Builder::set_stop` is
  `pub(crate)` — only `#[op]`-generated wiring reaches it. A sink that must do
  something *after the last cycle* (print a summary, flush a file) therefore
  goes through `PyStream::wire_op1_with_stop` /
  `Builder::register_op1_with_stop`. Do **not** reach for `ctx.is_last_cycle()`
  instead: it only fires for a cycle-bounded run in which that node happens to
  tick, so a duration-bounded, `Forever`, or aborted run silently skips it.
- **`cfg` is not reset between runs; `state` is.** If your op holds shared
  accumulator state in `cfg` (because a `#[pyclass]` handed back to Python
  shares it), clear it from the `state_init` factory — the engine re-runs that
  before a second `run()`, which is the only reset hook a `wire_op1` op gets.

## 5. Boundary rules

- **Attach the GIL once per burst, never per element.** `PyGraph::run`
  *detaches* for the duration of the run, so the graph thread does not hold the
  GIL while cycling and every `Python::attach` inside a cycle is a real
  `PyGILState_Ensure`/`Release` pair. Nested attaches short-circuit on a
  thread-local count, so hoisting one attach around a whole burst turns the
  per-element ones into the cheap path. The seams
  (`erase_burst_source` / `erased_burst_output` / `typed_burst_input`) already
  do this; if your binding does its own per-element Python work in a `map` or
  `try_map`, wrap the whole burst the same way.
- **Nothing holding a `Py<PyAny>` crosses to a worker thread** — see dynamic
  payloads above.
- **Real-time sources need the GIL released.** `PyGraph::run` does this. If you
  add a live-tail binding, cover it with an integration test that produces
  **from another Python thread mid-run** — that is the only test shape which
  catches a regression here. Without it, a live source only ever sees data that
  already existed, and every unit test still passes.

## 6. Tests — three tiers, all in the same PR

1. **Rust `#[cfg(test)]` marshaling tests** in the binding module: record →
   `dict`, `dict` → typed params, every error path, any query/frame
   construction. These run in `python-test.yml` via
   `cargo test --manifest-path crates/wingfoil-python/Cargo.toml --features all-adapters`.
2. **`tests/test_$ARGUMENTS.py`**, in two groups:
   - unit-level, **no service**, run by default: the module exposes the
     expected names, wiring constructs a `Stream`, optional args have defaults,
     wiring-time rejections raise, marshaling errors raise. Use an address that
     is guaranteed unreachable rather than a live one.
   - `@pytest.mark.requires_$ARGUMENTS` integration tests, **deselected by
     default** via the `addopts` in `pyproject.toml`. Register the marker there.
     They must **fail loudly** without the service, not skip.
   Give the module a docstring saying which group is which and how to start the
   service locally (a `docker run` line), as `test_postgres.py` does.
3. **A Python leg in `.github/workflows/$ARGUMENTS-integration.yml`**:
   start the service on its fixed port (the Rust tests use testcontainers, the
   Python ones need a known host/port), `maturin develop -F $ARGUMENTS`, then
   `pytest -m requires_$ARGUMENTS tests/test_$ARGUMENTS.py -v`. Add the binding
   and test paths to the workflow's `paths:` triggers. If the Rust leg already
   pins a fixed host/port (rather than testcontainers), reuse that instance
   instead of starting a second one — kdb's licensed container serves both legs.

   **When the adapter *is* the server** — prometheus binds its own HTTP
   endpoint, fix can accept its own initiator — there is no service to start,
   so the round trip belongs in the **default** tier, not behind a marker. Bind
   port 0, run the graph, then read the result back over loopback with the
   standard library. Mark it only if it needs a live wall clock (fix's sessions
   do; prometheus's scrape-after-run does not), and say in the module docstring
   why the marker exists, since "requires_x" otherwise implies a service.

   **When the adapter is a *client*, write the peer instead of a client.** The
   mirror of the rule above, and the cheaper side of the protocol to hand-roll:
   a binding that dials out needs something to dial, and a server that speaks
   just enough of the protocol for one well-behaved client is usually far
   smaller than a client would be. `ws`'s `_MiniWsServer` is ~70 lines of
   `socket` (handshake, short unmasked frames out, masked frames in) and needs
   **no marker, no package, and no workflow leg** — where `web`, whose binding
   *is* the server, needs a real WebSocket client and therefore the
   `websockets` package behind `requires_web`. Prefer this: a default-tier
   round trip is worth a lot more than a marked one nobody runs locally.

   Do not hand-derive a protocol constant from memory — take it from the crate
   the binding already depends on. The `ws` mini server's handshake failed
   against a correct-looking `Sec-WebSocket-Accept` because its RFC 6455 GUID
   was misremembered; the fix was reading `WS_GUID` out of tungstenite and
   pinning the RFC's published test vector.

   **Force a `gc.collect()` between tests that run Python threads.** `Graph` is
   an `unsendable` pyclass and a wired graph sits in a reference cycle, so it
   is freed by the *cyclic* collector — which runs on whichever thread happens
   to trigger it. One test's garbage graph then gets collected on a *later*
   test's server thread, and pyo3 raises `"unsendable, but is being dropped on
   another thread"`. It surfaces as a `PytestUnraisableExceptionWarning`
   attached to an unrelated, passing test, which is why it is worth knowing
   before you go looking. An autouse fixture is the whole fix:

   ```python
   @pytest.fixture(autouse=True)
   def _collect_graphs_on_the_main_thread():
       yield
       gc.collect()
   ```

   **When the service has no usable Python client**, do not add a heavyweight
   dependency or shell out to cargo. Speak enough of the wire protocol for
   *setup only* — kdb's `_q` is ~30 lines: handshake, one framed text query,
   raise if the reply is an error object — and route every **value** assertion
   back through the adapter under test, so the tier never has to decode a
   response.

   **When there is no I/O at all** — a pure in-process surface like `latency` —
   there is no third tier and no new workflow: everything runs by default in
   `python-test.yml`. Say so in the module docstring, since a reader who
   knows this skill will look for the marked tier and needs to know it is
   absent by design rather than forgotten.

## 7. Docs bookkeeping

- The binding module's `//!` header: the entry-point table (Python name → Rust
  fn → shape), how the dynamic edge works, and a numbered **deviations from the
  legacy `wingfoil-python` bindings** section. `postgres.rs` is the model.
- **`crates/wingfoil/src/adapters/$ARGUMENTS/CLAUDE.md`** — its
  `## Python` section. That is where an agent picking the adapter up cold looks
  for: the binding's cargo feature, whether it is in `all-adapters` and in the
  **wheel** (and why not, if not), the entry points it exposes and any Rust
  entry point it deliberately does *not*, whether it is `#[pyadapter]` or
  hand-written, the test file and its marker, and which workflow leg runs the
  marked tier. Every one of those is a fact this recipe made you decide — record
  it there rather than leaving it to be reverse-engineered from `Cargo.toml`.
- `docs/port-plan.md` — Phase 6, the "Per-adapter Python bindings" bullet:
  add `$ARGUMENTS` and keep the remaining count honest.
- `docs/python-interop.md` — the "Per-adapter Python bindings" row of the
  build-list table, same.
- `docs/deviation-register.md` — any cross-cutting legacy↔wingfoil difference.

## 8. Pre-commit checklist

**Run every command in the FOREGROUND and wait.** Do not background
`cargo lint-all` and move on — that is the most common way this work strands
with nothing committed.

```bash
cargo fmt --all
cargo lint
cargo lint-all
cargo test --manifest-path crates/wingfoil-python/Cargo.toml --features all-adapters
cd crates/wingfoil-python && maturin develop -F $ARGUMENTS && pytest -q
```

Sandbox caveat (same as `/new-adapter`): `cargo lint-all` is a workspace
all-features build and also compiles the legacy **aeron** C library, which
fails without the native toolchain. When that blocks you, substitute the scoped
equivalent and say so in the PR:

```bash
cargo clippy --manifest-path crates/wingfoil-python/Cargo.toml --features all-adapters --all-targets -- -D warnings
```

## 9. Self-review with a fresh context

Before opening the PR, run a clean-context review pass as a subagent:

1. Re-read this file, then walk `git diff next...HEAD` against steps 1–8 and
   produce a present / missing / diverged checklist.
2. Diff the binding against legacy `py_$ARGUMENTS.rs` one more time: every
   entry point, argument, and default → equivalent or a numbered deviation in
   the module docs.
3. Check the boundary rules in step 5 hold — especially the per-burst attach and
   that no `Py<PyAny>` reaches a worker thread.
4. Confirm the three test tiers exist, that the unit tier really needs no
   service, and that a live-tail binding has the cross-thread test. Confirm the
   adapter's `CLAUDE.md` `## Python` section matches what actually shipped
   (feature, roll-ups, entry points, marker, workflow leg).
5. Run the step-8 checklist and confirm every command passes.
6. Review for quality: no speculative abstraction, no dead code, no comments
   restating the code.
