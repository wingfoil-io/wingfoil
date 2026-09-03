# Adding an op — the tooling and the touch-points

The recipe for adding a node/op to the catalog (`ops.rs` / `stats.rs`), and the
table of what you actually have to touch. `/new-op`
(`.claude/commands/new-op.md`) is the step-by-step; this page is the reference
it defers to for *why* the boilerplate is shaped the way it is.

> **Extracted from [`planning/port-plan.md`](planning/port-plan.md) § Phase 2.**
> It lived there while the port was running, but the port is finished and that
> file is an archive — this is live reference material with callers across the
> skills, `crates/README.md`, `wingfoil-derive/README.md` and `ops.rs`, so it
> gets its own page. The design argument behind the no-table mechanism is
> [`decisions/macro-extensibility-decision.md`](decisions/macro-extensibility-decision.md).

## First: is it an op at all?

Some new vocabulary is not a new node. If the behaviour you want is an existing
op *re-spelled* — the same single node, the same `cycle`, differing only in how
the call site phrases it — it belongs in the fluent layer as a one-liner over
that op, not in the catalog. `filter_none` and `collapse_accumulate` are that
shape, and so is `filter_map` (#831): `Option`-shaped rather than `(B, bool)`,
but the same `MapFilter` node underneath.

The test is **node count**, not ergonomics: sugar wires exactly the node the
primitive would. A "sugar" method that wires *two* nodes (`.map(f).filter_none()`)
is a candidate op, because promoting it removes a node from every graph that
uses it. And a bound that looks like the sugar's cost usually isn't one — a
dedicated `FilterMap` op could not have dropped `B: Default`, since the `#[op]`
forwarders add `Out: Default` to every op for value-slot seeding.

The cost of choosing sugar is a real one and is paid in the macro crate: a
fluent-only method resolves fine when called but has no `__wf_op_<name>_*`
forwarders, so inside `nitro!` it fails on leaked internal symbols. It must be
named in `non_op_method_advice` (`wingfoil-derive`) with the primitive to spell
instead, pinned by a case in `tests/trybuild/fluent_only_sugar.rs`, and recorded
in the `tests/op_completeness.rs` allowlist. If that is not what you want, write
the op — see the promotion precedents (`not`, `collapse`, `count`,
`accumulate`, `merge_all`) in that allowlist.

## The recipe

Per node, in this order, no exceptions:

1. identify `Cfg` / `State` / `In<'a>` / `Out` / `ACTIVATION`;
2. write the `cycle` body — inputs are passed in, not read from upstream;
3. wire it up: `#[op(build = name)]` on the impl generates the interpreted
   `Builder` method from the op's `In` shape *and* the `nitro!`/compiled
   forwarders the emission dispatches through; add the fluent method (the one
   piece still hand-written);
4. write the tests as parity tests — values **and** tick times. If a legacy
   node is the twin, its unit tests are the oracle.

## Flush pending state on the final cycle

If an op holds pending state that represents a value the user would otherwise
never receive, flush it when [`Ctx::is_last_cycle()`](../crates/wingfoil/src/op.rs)
is true. `Window` and `Buffer` in `crates/wingfoil/src/ops.rs` are the worked
examples: each emits a final partial collection instead of silently dropping it
when a bounded run ends between its normal boundaries.

The flag deliberately does **not** propagate into a `nested()` island.
`Ctx::nested` reports `is_last_cycle: false` because the island owns its inner
schedule, so an op inside one flushes only on its own boundary or capacity —
never merely because the outer run has reached its final cycle. Pin both the
ordinary final flush and this island limitation with bounded historical tests,
including exact tick times.

## Why there is any per-op cost at all

Two mechanisms single-source most of the boilerplate; the residual per-op cost
is small and explained by two hard constraints on proc macros:

- **A proc macro sees tokens, not resolved types** — so `nitro!` cannot
  introspect an `Op` impl to learn its arity/cfg/input shape. Any per-op
  knowledge the macro needs must be written in the macro crate.
- **A trait cannot be extended from scattered sites** — so `#[op]` cannot add a
  method to `StreamOps` directly. It gets there anyway, one indirection later:
  `#[op(.., fluent)]` emits a `macro_rules!` writing the method, which the
  trait's `impl` block invokes. The **declaration** is still hand-written.
  (The same constraint is why the generated `Builder` method arrives on a *new*
  per-op trait rather than on one shared one — and why it can be generated at
  all outside this crate, where an inherent impl would be illegal.)

## What is automated

- **Interpreted engine** — `#[op(build = name)]` on `impl Op for X` generates
  `Builder::name` from the op's declared shape: one `Handle` parameter per edge
  of `In<'a>`, in order, then the `Cfg` (omitted when it is `()`). This covers
  **every** shape the macro parses — sources (`In = ()`), single- and
  multi-input ops, edges read with their tick flag (`(&'a T, bool)` — `delay`,
  `merge`), `passive = [..]` non-activating edges, `start`/`stop`/`teardown`
  lifecycle hooks, and `init_arg` seeded accumulators — so no op keeps a
  hand-written `Builder` method for want of tooling. Node labels come from
  `type_name::<X>()` (shortened), not hand-written strings. `no_builder` is
  left for the case where the interpreted *signature* deliberately differs
  from the shape: `with_time` (seeds its value slot from the input's current
  value, so it never requires `Out: Default`) is the catalog's only one. The
  `bimap`/`trimap` family are *additional* hand-written methods over the
  `Join`/`Join3` ops — their active/passive split is a runtime argument rather
  than the compile-time `passive` mask — alongside the generated `join` /
  `join_passive` / `join3`. The generated body is the same
  `next_node_index` → `slot`/`new_slot` → `push_node` → `set_*` sequence a
  hand-written builder contains, against a `#[doc(hidden)] pub` codegen seam on
  `Builder`. `register_op1`…`register_op4` remain the curated, documented
  primitives for wiring a shape *by hand*.
- **Compiled / `nitro!`** — **zero-touch, because there is no per-op table.**
  `#[op]` emits forwarder functions by naming convention (`__wf_op_<name>_*`)
  and per-op facts (`ACTIVATION`, passive-edge masks) as consts the emission
  folds on; rustc's inference resolves the op type the macro never names. The
  `OpKind`/`OpInfo` table this bullet used to describe has been **deleted** —
  built-in ops and user ops now take the identical path.

## Out-of-crate ops take the identical path

`#[op]` is **not** in-crate tooling. Its expansion names `::wingfoil::…`
throughout and hangs the generated `Builder` method on a per-op extension trait
(`__WfBuild<CamelName>`) implemented for `wingfoil::interp::Builder`, so the
same attribute expands the same way in a downstream crate
([#782](https://github.com/wingfoil-io/wingfoil/issues/782)). A user op is:

```rust
use wingfoil::op;                              // the attribute
use wingfoil::op::{Activation, Ctx, Op, Tick}; // the trait
use wingfoil::prelude::*;

pub struct Gain;

#[op(build = gain)]
impl Op for Gain { /* Cfg / State / In / Out / ACTIVATION / cycle */ }

trait UserOps { fn gain(&self, factor: f64) -> Stream<f64>; }

impl UserOps for Stream<f64> {
    fn gain(&self, factor: f64) -> Stream<f64> {
        self.wire(move |b, h| b.gain(h, factor))
    }
}
```

and that reaches `interpreted()`, `compiled()` and `nested()` alike. Two
things to know, neither specific to the catalog:

- **The generated method needs its trait in scope.** In the op's own module
  that is automatic; from another module it takes
  `use path::to::__WfBuild<CamelName>;`. In-crate, that is why `fluent.rs`
  and `adapters::statistics` glob-import `crate::ops` — naming ~70
  traits one by one is churn with no reader value.
- **The dependency must be named `wingfoil`.** The expansion is
  `::wingfoil::`-qualified, so a crate that renames it
  (`wf = { package = "wingfoil", … }`) cannot use `#[op]` — or `nitro!`, which
  has always emitted the same paths. The workaround is
  `extern crate wf as wingfoil;` at the dependent's crate root. There is no
  `$crate`-style fix available: a proc macro cannot learn what the dependent
  calls its dependencies.

Worked examples: `crates/wingfoil/tests/custom_op.rs` (an integration test is a
separate crate, so every `#[op]` in it is an out-of-crate expansion) and
`crates/wingfoil/tests/trybuild/pass/out_of_crate_op.rs` (compiled and run as
its own crate in a throwaway Cargo project, outside the workspace).

## The touch-point table

Where to touch when adding an op — **the compiled path is zero-touch**:

| Op shape | Interpreted | `nitro!`/compiled |
|---|---|---|
| Single-input | `ops.rs` (`impl` + attr) + fluent method | nothing — `#[op]`'s forwarders cover it |
| Multi-input, values-only, all-active (the `join` shape) | same — `ops.rs` (`impl` + attr) + fluent method | nothing — `&stream` args classify as edges |
| Source (`In = ()`), lifecycle hooks, tick-flag edges | same — `ops.rs` (`impl` + attr) + fluent method | nothing |
| Passive edges (`passive = [..]`) / seeded accumulators (`init_arg`) | same — `ops.rs` (`impl` + attr, with the flag) + fluent method | nothing — attribute flags on `#[op]` |
| Interpreted signature ≠ the op's shape (`with_time`) | `no_builder` + a hand-written `Builder` method + fluent method | nothing |
| Side-effect sink (`print`, `for_each`) | same — and leave the combinators' `#[must_use]` off the fluent declaration | nothing |

Two things ride on the fluent *declaration* being hand-written, and
`#[must_use]` is now one of them: a discarded combinator result is not a no-op
(the node stays wired and cycles every tick), so every transform and source
declaration carries `#[must_use = "…"]`. It has to sit on the declaration —
inside the `__wf_fluent_*` expansion it would land in a trait `impl`, where the
attribute is inert. Sinks are the exception and carry nothing. See `/new-op`
step 4c.

Constraint #1 still holds (a proc macro sees tokens, not types), but it is
routed around rather than paid per-op. Delay's engine-level special cases became
`Tick::Silent` in the `Op` contract. Measured at parity with the deleted table
emission and covered by `wingfoil/tests/custom_op.rs`; full analysis in
[`decisions/macro-extensibility-decision.md`](decisions/macro-extensibility-decision.md).
The fluent method remains hand-written (constraint #2, unchanged).

## The completeness guard

There is no central op list to diff, so the guard is realized at **compile
time** in `tests/op_completeness.rs`: a combinator *used inside* a `nitro!`
block only compiles if it has **both** a fluent method (the wiring fn is fluent
code) **and** a forwarder (`#[op]`), so exercising every dual-mode combinator
there is exactly the two-sided one-sided-registration guard. Each block
additionally asserts `interpreted() == compiled()`.

The by-design fluent-only surface — feedback, the IO sources
(`external`/`channel`/`poll`), the `for_each` sink — is documented in that file
as an explicit allowlist. Three further ops are interpreted-only *for want of a
forwarder* rather than by design — `join_passive` / `try_join_passive`,
`delay_with_reset`, and `with_time` (all hand-written builder methods, not
`#[op]`). Those are candidate follow-ups, not by-design gaps.

Two shapes genuinely cannot be promoted: `split` returns two output streams
against an `Op`'s single `Out`, and `never` has no `Op` witness for `#[op]` to
hang forwarders on (a source that never ticks). A runtime-width fan-in
(`combine`, `merge_n`) defeats `#[op]`'s *generation* but not the emission —
both reach all three engines through a witness op with hand-written forwarders
declaring `In<'a> = &'a [(&'a T, bool)]`.
