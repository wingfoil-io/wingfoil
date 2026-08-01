Implement a new node/op for **wingfoil-next** named `$ARGUMENTS`, in the op
catalog (`next/crates/wingfoil-next/src/ops.rs`, or `stats.rs` for a
statistics op). Follow these steps in order. Work test-driven: write each
parity test before its implementation.

Ops in next are **associated functions on a zero-sized witness type**, never
methods on an instantiated object — the semantics are written once and executed
by every engine (interpreted, compiled, nested). The existing ops are the
reference implementations; read them before writing code:

- `src/ops.rs` — the core catalog. `Map` (`#[op(build = map)]`, the smallest
  single-input shape), `Fold` (`init_arg` seeded accumulator), `Ticker` /
  `Const` (`no_builder` sources), `Sample` (`passive = [0]`), the multi-input
  `bimap`/`trimap`/`join` family.
- `src/stats.rs` — `StatisticsOps`, the template for an op that lives in its
  own extension trait outside the prelude (EWMA family).
- `src/op.rs` — the `Op` trait itself: `Cfg` / `State` / `In<'a>` / `Out` /
  `ACTIVATION`, and `Tick<T>` (`Value` / `Silent` / `Quiet`).
- `src/fluent.rs` — `StreamOps` / `SourceOps`, where the fluent method lives
  (a one-liner over `Stream::wire` / `GraphBuilder::source`).
- `crates/wingfoil-next-macros/src/lib.rs` — the `#[op]` macro and its flags.
- `next/docs/port-plan.md` → **"Adding an op — current tooling"** — the
  authoritative recipe and the touch-point table; read it first.

## The parity obligation (read first)

Wingfoil Next's governing design objective (see `next/README.md` and
`next/CLAUDE.md`) is to become a **strict superset of legacy wingfoil**. If a
classic node named `$ARGUMENTS` exists under `wingfoil/src/nodes/`, it is your
**parity oracle**:

- Read its `MutableNode` impl and its unit tests first.
- Move the classic `cycle` body **verbatim** into the op — same logic, with
  inputs passed in per cycle (`In<'a>`) instead of read from upstream `Rc`s.
- Every public capability (config knob, mode, tick-suppression rule) needs a
  next equivalent, or an explicit deviation note in the op docs and, if it's a
  capability gap, in the capability matrix / inventory in `port-plan.md`.
- Port its unit tests as parity tests: identical values **and** tick times.

If no classic node exists you are defining new surface: keep the naming and
layering conventions below so a future legacy backport stays mechanical.

## Feed lessons back into this skill

Op development keeps surfacing things this skill doesn't yet capture — a
recurring pitfall (the `Fn`-not-`FnMut` closure-config contract; a
`Tick::Silent` vs `Quiet` subtlety), a shape that doesn't fit `#[op]`, a CI
gate you didn't expect, a pattern worth codifying. **When you hit one, bake it
into this file** (`.claude/commands/new-op-next.md`), ideally in the same PR, or
flag it for a follow-up skill update. This skill is meant to grow with every
op ported — the same way `/new-adapter-next` grew most of its rules. Record
cross-cutting classic↔next differences in `next/docs/deviation-register.md`.
**Changing an existing op counts too:** if a change invalidates or extends a
rule here, update the rule in the same PR. A skill that has drifted from how we
actually add ops is a bug.

## 1. Branch

**All next work cuts from and merges into `next`, never `main`** (see
`next/CLAUDE.md`). Cut the feature branch from `next`:

```bash
git checkout next && git pull origin next && git checkout -b $ARGUMENTS-op-next
```

When you open the PR, its **base branch must be `next`** — not `main`.

## 2. Classify the op shape — the load-bearing decision

The shape decides how much you write and which engines you reach for free.
Read the touch-point table in `port-plan.md` ("Adding an op"); the summary:

| Shape | `In<'a>` | Interpreted (`ops.rs`) | `graph!` / compiled | Reference |
|---|---|---|---|---|
| **Single-input** | `(&'a I,)` | `#[op(build = name)]` — generates `Builder::name` over `register_op1` | **zero-touch** — `#[op]` emits the forwarders | `Map`, `Filter`, `Distinct` |
| **Seeded accumulator** | `(&'a I,)` + init | `#[op(build = name, init_arg)]` (implies `no_builder`) + hand `Builder` method | zero-touch — attribute flags carry it | `Fold` |
| **Passive edge** (read, don't trigger) | `(&'a I,)` | `#[op(build = name, passive = [0])]` | zero-touch | `Sample` |
| **Multi-input, all-active** | `(&'a A, &'a B)` | `#[op(build = name, no_builder)]` + `register_op2`-based fluent method | zero-touch — `&stream` args classify as edges | `bimap` / `join` |
| **Source** (no input) | `()` | `#[op(build = name, no_builder)]` + hand `Builder` method that `schedule`s | fluent-only if it's a cycle/IO source | `Ticker`, `Const` |
| **Doesn't fit** (3+ inputs, tick-flag inputs, lifecycle hooks, custom state seed) | — | hand-written `Builder` method | add by hand or leave interpreted-only | see `port-plan.md` |

The two hard constraints behind this (from `macro-extensibility-decision.md`):
a proc macro sees **tokens, not resolved types**, so `graph!` can't introspect
an `Op` impl; and a trait **can't be extended from scattered sites**, so the
fluent method is always hand-written. Everything else is generated.

**Scheduling / activation.** Set `const ACTIVATION` from behaviour, not habit:

- `Activation::NONE` — pure transform, ticks only when an active input ticks
  (`map`, `fold`).
- `Activation::SCHEDULES` — time-gated / self-scheduling; call `ctx.schedule(t)`
  (`ticker`, `delay`, `throttle`).
- `Activation::ALWAYS` — runs every cycle (`always`).
- `Activation::THREADED` — fed from a background thread / external waker
  (channel / external sources — adapter territory, see `/new-adapter-next`).

**`Tick` variant** is a correctness contract, not a style choice:
`Tick::Value(v)` ticks downstream; `Tick::Silent(v)` updates the value slot
**without** ticking (what `delay` needs so a passive reader never sees
`T::default()`); `Tick::Quiet` emits nothing (warm-up, suppressed duplicate).

## 3. Implement the `Op`

In `ops.rs` (or `stats.rs`), add a zero-sized witness type and its `impl Op`:

```rust
/// <one line: what it computes, and the tick-suppression rule>. <If porting:
/// "Ports classic `wingfoil::nodes::$ARGUMENTS`.">
pub struct MyOp<A, B>(PhantomData<(A, B)>);

#[op(build = $ARGUMENTS)]
impl<A, B> Op for MyOp<A, B>
where
    A: 'static,
    B: Clone + 'static,
{
    type Cfg = /* construction-time config; closures live here */;
    type State = /* engine-owned mutable state; must be Default */;
    type In<'a> = (&'a A,);
    type Out = B;
    const ACTIVATION: Activation = Activation::NONE;

    fn cycle(
        cfg: &mut Self::Cfg,
        state: &mut Self::State,
        input: (&A,),
        ctx: &mut Ctx<'_>,
    ) -> Result<Tick<B>> {
        // classic cycle body, verbatim; inputs passed in, not read from Rc<dyn Stream>
    }

    // optional: fn start(...) -> Result<()> to seed State / convert Cfg once
    // (per-cycle conversion measurably slows dense graphs — see Ticker::start).
}
```

Rules the existing ops encode — follow them:

- **Closure configs are `Fn`, never `FnMut`.** Compiled expansions re-create
  closure configs per cycle, so a closure mutating its captures would drift
  (silently reset compiled, persist interpreted). `Fn` makes that a compile
  error in both. Per-node mutable state belongs in `State`, not a closure
  capture. (See the `Map` doc comment for the full rationale.)
- **`State: Default`** — the interpreted `#[op]` path seeds it with
  `Default::default()` per run (re-seeded each run, so a graph re-runs clean).
  A non-`Default` seed needs a hand-written `Builder` method (`no_builder`).
- **Convert config once in `start`**, not per `cycle`, when the conversion is
  non-trivial (`Ticker` converts its `Duration` period to `NanoTime` in
  `start`).
- **Validate user config** at wiring when possible; when it needs runtime info,
  `anyhow::bail!` inside `cycle` with a clear message (aborts the run). **Never
  panic** for bad user config. No `.unwrap()` outside `#[cfg(test)]` / doc
  examples (repo-wide rule).
- **Doc every public item.** `#[op]`-scoped ops get a `Builder::$ARGUMENTS`
  method whose docs are the witness type's docs — write them for a caller.

## 4. The fluent method — hand-written, out of nothing generated

The `#[op]` macro **cannot** add a method to a trait (constraint #2), so add
the fluent combinator by hand — a one-liner over `Stream::wire` (or
`GraphBuilder::source` for a source). Put it on the right trait:

- General combinator → `StreamOps` in `fluent.rs` (declare in the trait, impl in
  the `impl<T> StreamOps<T> for Stream<T>` block):
  ```rust
  fn $ARGUMENTS<B, F>(&self, /* args */) -> Stream<B>
  where B: Clone + Default + 'static, /* … */
  { self.wire(|b, h| b.$ARGUMENTS(h, /* args */)) }
  ```
- Multi-input → `self.wire(|b, h| b.$ARGUMENTS(h, other, /* … */))` over
  `register_op2` (see `join` / `bimap`).
- Statistics / domain op → its own extension trait kept **out of the prelude**
  (`StatisticsOps` in `stats.rs`); users opt in with
  `use wingfoil_next::stats::StatisticsOps;`, mirroring adapters.

A source's fluent method goes on `SourceOps` and calls `GraphBuilder::source`
/ the generated `Builder` method.

## 5. `graph!` / compiled coverage

For a single-input `#[op]` op this is **zero-touch**: the attribute emits the
forwarder functions (`__wf_op_<name>_cycle`, `__WF_OP_<NAME>_ACTIVATION`) that
compiled/nested emission dispatches through by naming convention, and rustc's
inference resolves the op type the macro never names. Nothing to edit.

For a shape outside `#[op]`'s scope (multi-input beyond `register_op2`,
tick-flag inputs, cyclic/IO sources), the op may land **interpreted-only** —
that is allowed, but it must be *consciously* placed: either give it an
equivalent forwarder, or add it to the documented fluent-only allowlist in
`tests/op_completeness.rs` (see step 6). Never leave it silently in neither.

**Gotcha — an ergonomic fluent signature that differs from the op's `Cfg`
forces fluent-only.** `graph!`/compiled emission uses the **call-site argument
types verbatim** as the op's `Cfg` (a plain arg → `__cfg` local, tuple in call
order), then hands them to `__wf_op_<name>_cycle(__cfg: &mut <Cfg>)`. So a
call-site type must *equal* the `Cfg` type. If the fluent method takes a
different, more ergonomic type and converts — the classic pattern being a
`&str` label the method turns into an owned `String` `Cfg` (`logged`) — the
**same tokens cannot satisfy both**: `wire()`/interpreted wants the `&str`
fluent param, compiled wants the `String` cfg. Such an op stays **fluent-only**
even though `#[op]` emits its forwarders (harmless, unused). Options: (a) accept
it as fluent-only and record it in the `op_completeness.rs` allowlist (category
"ergonomic fluent signature ≠ `Cfg`"); or (b) make the `Cfg` *be* the
call-site type (e.g. `Cfg = (&'static str, …)`) — only viable when a borrowed,
`'static` config is acceptable, which it usually is not (dynamic `format!`
labels need an owned `String`). `logged` took (a).

## 6. Tests

### Parity / catalog tests — `tests/catalog*.rs`

Mirror the classic node's own unit tests. Conventions (see `tests/catalog.rs`,
`tests/catalog_ops.rs`, `tests/catalog_flow.rs`):

- Run historical for determinism: `RunMode::HistoricalFrom(NanoTime::ZERO)`.
- Assert **values and tick times** — build the graph, `r.run(...)`, and check
  `r.value(&stream)`; use `.with_time()` / `.accumulate()` to capture tick
  timing, not just the final value. Tick **suppression** (an op that goes
  `Quiet`) is part of the contract — assert the suppressed ticks are absent.
- Port every classic unit test first, then add next-specific cases.

### Completeness / engine-parity guard — `tests/op_completeness.rs`

This is a **compile-time** guard against one-sided registration: a combinator
used inside a `graph!` block only compiles if it has **both** a fluent method
**and** a forwarder. So:

- **Dual-mode op** → add it to a `graph!` block here; each block also asserts
  `interpreted() == compiled()`, extending engine-parity across your op.
- **Deliberately fluent-only op** (IO/cyclic source, or a not-yet-forwarded
  shape) → add it to the documented allowlist in this file with a one-line
  reason. Adding an op means consciously choosing one of these — never
  silently neither.

Compiled-specific stateful/lifecycle behaviour has its own suites
(`tests/compiled_stateful_ops.rs`, `tests/compiled_lifecycle_ops.rs`,
`tests/nested_islands.rs`) — extend the relevant one if your op is stateful or
has `start`/`stop` hooks.

### Python bindings — see step 7

## 7. Python bindings (`wingfoil-next-python`)

`wingfoil-next-python` is the **go-forward** Python binding (it supersedes
legacy `wingfoil-python`; see `next/docs/python-interop.md`). Everything
Python-composable rides one erased edge type, `PyElement` — **only the edges
erase**, the op interior stays natively typed. pyo3 forbids `#[pymethods]` on a
foreign pyclass, so a user op becomes a **free `#[pyfunction]`**
(`module.$ARGUMENTS(stream, …)`), not `stream.$ARGUMENTS(…)` — the same shape
polars expression plugins use.

Pick the lightest tool that fits:

- **Stateless single-input** (with or without one config arg) → the
  `pyop_fn!` declarative macro (`crates/wingfoil-next-python/src/macros.rs`):
  ```rust
  pyop_fn! {
      /// <doc>
      fn $ARGUMENTS(cfg: f64): f64 => f64 = |cfg, _state, a, _ctx| Ok(Tick::Value(/* … */))
  }
  ```
- **Any concrete one-, two- or three-input op** → the `#[pyop]` **proc**
  macro (`wingfoil-next-python-macros`), placed alongside `#[op]` on the `Op`
  impl; it reads the associated types + `cycle` and emits the `#[pyfunction]`:
  ```rust
  #[op(build = $ARGUMENTS)]
  #[pyop(name = $ARGUMENTS)]
  impl Op for MyOp { /* … */ }
  ```
  Covers stateless and stateful ops (`State` is any `Default`-seedable type,
  re-seeded per run) at one to four inputs — `In<'a> = (&A, &B)` emits
  `module.name(stream, other)`, `(&A, &B, &C)` emits
  `module.name(stream, second, third)`, and four inputs adds `fourth`. All
  inputs are active; a passive edge still needs a hand-written method. The
  stream parameters are named, so callers may pass them by keyword.

  A tuple `Cfg` gets one named Python parameter per element:
  ```rust
  #[pyop(name = zscore, arg = (window, decay))]   // Cfg = (usize, f64)
  ```
  which reads as `zscore(stream, window, decay)` instead of taking a tuple.

  **Arity 5+ is not a macro gap — it is a missing primitive.** Add
  `Builder::register_op<n>` (mirror `register_op4`, which mirrors
  `register_op3` line for line), `PyStream::wire_op<n>`, and the parameter name
  in the macro's `receiver_names`; the emitter itself is arity-generic. Each
  arity needs its own registration function because the inputs are
  heterogeneous static types and Rust has no variadic generics — which is a
  limit on *Rust-authored* ops only. A node authored in Python via
  `Graph.custom_node` / `CustomStream` takes any number of erased upstreams.

Then:

1. **Register** the generated function in the `#[pymodule]` in
   `src/python.rs`: `m.add_function(wrap_pyfunction!($ARGUMENTS, m)?)?;`.
2. **Edge conversions**: `PyElement <-> f64/i64/bool/String` already ship; a
   custom value type needs its own `From`/`TryInto` impls at the edge only.
3. **Rust seam test** in `tests/plugin_seam.rs` — wire the op over
   `wire_op1`/`wire_op2` and assert values + tick times, the same parity
   discipline as everywhere in next.
4. **pytest** in `tests/test_interop.py` — call `wingfoil_next.$ARGUMENTS(...)`,
   compose it between built-in combinators, and assert the result. Include a
   round-trip that also authors the same graph purely in Rust and asserts they
   agree, when practical (the parity-oracle discipline).

If the op is not (yet) Python-exposed, say so explicitly in the PR description
so reviewers don't flag it as missing — not every internal op needs a binding,
but the choice should be stated.

## 8. Roadmap bookkeeping

Update `next/docs/port-plan.md`: mark `$ARGUMENTS` in the Phase 2 inventory
table (✅/🟡 with the test-file name), matching how the existing catalog rows
read. If the op is interpreted-only by necessity, note it in the "engine
coverage" paragraph as a candidate follow-up, not a silent gap.

## 9. Pre-commit checklist

**Run every command in the FOREGROUND and wait for it to finish.** Do NOT
background `cargo lint-all` and move on — backgrounding it then ending the turn
is the single most common way this work strands with nothing committed. One
command at a time, blocking, until it returns.

```bash
cargo fmt --all
cargo lint                                   # default features
cargo lint-all                               # all features (needs protoc)
cargo test -p wingfoil-next                  # catalog + completeness + parity
# if you touched Python bindings:
cargo test -p wingfoil-next-python           # the Rust seam tests
cd next/crates/wingfoil-next-python && maturin develop && pytest
```

All must pass before committing. `cargo lint-all` is what CI runs — it is the
only lint pass that sees feature-gated code (e.g. a `stats`/`augurs` op).

**Sandbox caveat** (same as the adapter skill): `cargo lint-all` is a workspace
all-features build, so it also compiles the classic **aeron** C library, which
fails in a dev sandbox without the native toolchain — unrelated to your change.
When that blocks you, run the scoped equivalent that still lints every
`wingfoil-next` feature/target:

```bash
cargo clippy -p wingfoil-next --all-features --all-targets -- -D warnings
```

Note the substitution in the PR; the full workspace `lint-all` runs in CI.

## 10. Self-review with a fresh context

Before opening a PR, run a clean-context review pass as a subagent:

1. **Re-read this skill end to end**, then walk `git diff next...HEAD` against
   steps 1–9 and produce a present / missing / diverged checklist. Flag every
   divergence, even intentional ones.
2. **Validate the artifacts**: branch cut from `next`, PR base `next` (step 1);
   the `Op` impl with correct `ACTIVATION` and `Tick` variants (steps 2–3);
   closure configs are `Fn` not `FnMut`; `State: Default` (or a justified
   `no_builder`); a hand-written fluent method on the right trait, out of the
   prelude for a domain op (step 4); `graph!`/compiled coverage is zero-touch or
   the op is a documented fluent-only entry (step 5); catalog tests assert
   values **and** tick times and the op appears in `op_completeness.rs` (a
   `graph!` block or the allowlist) (step 6); Python binding + registration +
   seam test + pytest, or a stated reason there's none (step 7); port-plan
   updated (step 8).
3. **Check parity**: diff against the classic node — every classic test has a
   next twin with identical values and tick times; the deviations list in the
   op docs is complete.
4. **Run the pre-commit checklist from step 9** and confirm every command
   passes. Do not skip any.
5. **Review for quality**: no speculative abstractions, no dead code, no
   comments restating the code, no half-finished paths.

Fix everything found before committing. A clean self-review is part of "done".
