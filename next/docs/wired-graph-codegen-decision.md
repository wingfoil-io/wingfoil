# Two-pass codegen from a wired graph — the `func!` quotation design

**Status: accepted direction, not yet implemented.** This records the design
and its reasoning; the build is gated on the sequencing plan in §8 (in
particular, on a named workload that needs it). Companion to
[`macro-extensibility-decision.md`](macro-extensibility-decision.md), which
this document extends — read that first for the forwarder mechanism it
builds on.

**Question.** Instead of (or alongside) the `graph!` macro wrapping the
wiring, could we generate compiled-graph code by *running* the wiring —
traversing the wired (interpreted) graph — and emitting source that a second
compilation pass turns into the monomorphized runner?

**Answer.** Yes — for a well-defined subset, and only with one new
primitive: **`func!`, user-level quotation of closures**. The subset is
"closures are closed or carry an explicit, literal-emittable capture list;
configs are literal-emittable data". Positioned correctly, this is not a
replacement for `graph!` but a **second front-end into the same emission
mechanism**, and it buys the one thing `graph!` structurally cannot do:
**dynamic topology** — graphs whose shape is decided by running code
(config files, discovered instrument lists) — at compiled speed. `graph!`
remains the single backend: the generator's output artifact *is* a plain
wiring fn wrapped in `graph!`.

---

## 1. Why naive traversal fails (and failed)

A wired graph can give back its **topology** by traversal: node count, op
identity (the `#[op]` build name), edge lists, active/passive flags,
activation, creation order — all type-free facts the interpreted `Builder`
holds. What it cannot give back is anything type- or value-level. The
interpreted engine erases exactly what an emitter needs: each node's cycle
is a `Box<dyn FnMut(&mut Kernel) -> Result<bool>>` adapting the monomorphic
`Op::cycle`, and value slots are `Rc<dyn Any>` (`interp.rs`).

The repo has lived through the naive version once: the classic
`wingfoil::codegen` retrofit (since deleted; its walls are recorded in
`wingfoil-next/src/lib.rs`) ran the wired graph from `build.rs` and emitted
a runner. Types came back as name strings; closures could not be recovered
at all, forcing the `Inputs` re-supply — a human re-stating each closure —
and its drift risk. Wall #1 was even more fundamental: with semantics
trapped in `MutableNode` objects, the emitted runner had to *re-implement*
node semantics as strings.

Thinking past the naive version, the type problem is solvable without
naming types — the same way `graph!` solves it. Emission can dispatch
through the naming-convention forwarders (`__wf_op_<name>_cycle`) with
typeless `Default::default()` state locals, and rustc's inference
monomorphizes the chain. Configs don't need to be *contained* in generated
code either, only *rebound*: the generated runner could re-invoke the
user's wiring fn against a replay-checked capture builder and pick each cfg
up in creation order.

That design gets 90% of the way and dies at a single, precise point:
**handing a captured closure cfg to its forwarder call.** Storing a dynamic
number of heterogeneous cfgs forces erasure into `Box<dyn Any>`; getting
one back out requires `downcast` with a type that is either *nameable*
(closure types aren't) or *inferable* — and it isn't: in the macro path the
closure **literal** sits at the call site and roots the inference, but in
generated code the closure's type `F` appears only as the projection
`<SomeOp<F> as Op>::Cfg`, and nothing else in the program constrains it.
So post-wiring traversal works for every op *except* the closure-configured
ones — `map`, `filter`, `fold`, `join` — i.e. the entire useful surface.

Named for what it is: this is the multi-stage-programming problem
(LMS/MetaOCaml — "run the program, emit a specialized program"). Those
systems handle closures only by making user functions operate on staged
`Rep<T>` values, i.e. by changing the surface language. Rust has no
quote/run staging. The `graph!` macro is not an arbitrary alternative to
two-pass codegen — it is the *minimal* device that keeps a token of each
closure where emitted code can re-mention it.

## 2. The missing primitive: `func!` quotation

A macro wrapped around the closure **at its call site** sees the closure's
tokens before type-checking and erasure, and can preserve exactly what
post-wiring traversal cannot recover:

```rust
// func!(|x| x * 10) expands to roughly:
QuotedFn {
    f: |x| x * 10,        // the real closure — interpreted path uses this, zero cost
    src: "| x | x * 10",  // stringify!(...) of the same tokens
    loc: /* file!(), line!() — for generator error mapping */
}
```

The builder stores `src` into type-free node metadata at wiring time. This
kills both blockers from §1 at once:

1. **Source recovery** — the traversal reads `src` off each node and
   splices it into the generated wiring fn as a closure *literal*.
2. **Inference rooting** — the emitted text is a closure literal at the
   *generated* call site: exactly the inference root `graph!` relies on.
   Pass 2 needs no type names anywhere; it emits the same forwarder calls
   and typeless state locals the macro emits today, and the measured
   1.01×-vs-hand-written result carries over unchanged.

And a third property classic codegen never had: **drift between the value
that ran and the text that was emitted is structurally impossible**,
because they are the same tokens by construction. The macro guarantees
text ≡ behavior; wall #2's failure mode (a human re-stating the closure
wrongly) cannot exist.

## 3. The capture rules

The stringified body may reference names from the original scope
(`|x| x * threshold` over a wiring-fn local). Spliced elsewhere, that
either fails to compile or — worse — resolves to a *different* name. So
`func!` must enforce closedness, in two tiers:

- **Tier 1 — no captures**, enforced mechanically: the expansion coerces
  through a fn pointer (`let f: fn(_) -> _ = |x| x * 10;`). Non-capturing
  closures coerce; capturing ones fail at the original call site.
- **Tier 2 — explicit capture list**: `func!([thresh] |x| x.abs() > thresh)`.
  The macro stringifies the body, records the capture *values* (bounded by
  an `EmitLiteral`-style trait: primitives, `String`, `Duration`,
  `NanoTime`, `Vec<primitive>`, …), and the generator re-materializes them:
  `{ let thresh = 0.25f64; move |x| x.abs() > thresh }`.

Tier 2 is the [`serde_closure`](https://crates.io/crates/serde_closure)
pattern (`Fn!` with explicit captures, used to ship closures across process
boundaries) — proven prior art.

**Design fork, decided per use case, defaulting to freezing:** emitting
capture values as literals *freezes* them — pass-1's config is baked into
the binary. That is partial evaluation (often the point: specialization),
but it must be framed loudly ("this bakes today's config into the binary")
or someone ships a stale threshold. The alternative — promoting captures to
parameters of the generated wiring fn so the final program supplies fresh
values — keeps config live at the cost of requiring nameable capture
types; it is a follow-up, not v1.

Non-closure configs need the same treatment: data cfgs (durations,
endpoints, thresholds) must be `EmitLiteral`. Runtime resources (channel
receivers, sockets) are fine *because the output is a wiring program, not a
state dump*: the generated wiring fn re-creates them fresh in the final
process, and next's defer-to-start lifecycle keeps wiring side-effect-free.

## 4. Regular closures stay first-class

`map` and friends accept both forms through one trait. Ops take
`F: OpFn<...>` instead of `F: Fn(...)`: a blanket impl for ordinary
closures (reporting "no source"), a specific impl for `QuotedFn` (reporting
its `src`). No overlap on stable Rust — `QuotedFn` can't implement `Fn`
anyway, so ops invoke the cfg through the trait's own call method. The
`#[op]`-generated builder method asks `cfg.src()` once at wiring time; at
runtime the wrapper is transparent (the src is dead data post-mono, the
call inlines).

The key asymmetry: **`graph!` never needs `func!`** — the macro has the
tokens already. Quotation is only the *generator's* requirement, because it
meets the closure after erasure. So plain closures keep everything that
exists today: full interpreted support, full `compiled()`/`nested()`
performance, implicit environment capture. `func!` is purely additive
vocabulary for wiring that wants to be generator-eligible (and is harmless
inside `graph!`, where it's just another config expression).

When the generator meets an unquoted closure:

1. **v1 — loud, precise failure.** Record `Location::caller()` on wire
   calls (`#[track_caller]`); the generator errors with the exact list of
   non-emittable nodes and their call sites.
2. **v2, deferred — mixed mode.** Since the final program links the
   original wiring fn, the generated runner can re-run it against the
   interpreted builder in the final process and drive *opaque* nodes
   through their existing `Box<dyn FnMut>` cycle adapters — self-contained
   because they're constructed inside the generic `wire` call where types
   are known. Quoted nodes get monomorphic emission; unquoted ones cost
   one dyn call each: performance degrades per node, not per graph (the
   mirror image of `nested()`). Real engineering at the seams (typed reads
   from an opaque node's erased slot lean on inference rooted in the
   downstream quoted closure), hence deferred.

## 5. Pipeline, artifact, and scope

The wiring for a dynamic graph is a **plain Rust function** — no macro
around it (its shape depends on runtime data), quoted closures inside it.
Day-to-day development runs it interpreted: no build step, edit-and-rerun.

A generator step (a small bin or `build.rs`) runs the same wiring once
against the recording builder and prints the artifact:

```rust
wingfoil_next::codegen::generate(|g| desk_graph(g, &config), "src/desk_graph.gen.rs")?;
```

The artifact is **reviewable plain Rust: a `graph!` invocation with the
topology unrolled** and captures frozen as literals (each carrying a
`// from wiring.rs:NN` breadcrumb from the recorded `loc`). A wiring loop
over N configured symbols becomes N unrolled pipelines — pass 1 ran the
loop, so the generator emits exactly the shape it produced.

**Scope convention:** the final program includes the artifact **in the
module where the wiring lives** (`include!`), so spliced bodies resolve
against their original scope — imports, helper fns, types. This is the
design's roughest edge in practice: wiring split across modules breaks it,
and pass-2 compile errors land in generated code (mitigated by the
breadcrumbs). Convention: quoted wiring lives in one module; body paths
that must resolve elsewhere are crate-rooted (`crate::` / `::std::`).
Failures are loud pass-2 compile errors, never silent misbehavior.

Determinism obligations of any two-pass design apply: pass-1 wiring must be
deterministic and re-runnable in the generation environment (defer-to-start
makes wiring side-effect-free, which is most of this), and stale-generation
is a standing hazard — mitigated by making the artifact `graph!` input
(diffable, reviewable) rather than expanded runner code.

## 6. Why `graph!` stays — and why the Op rearchitecture is the prerequisite

`graph!` keeps three roles under this design:

1. **The generator's backend.** Emitting `graph!` input instead of expanded
   runners means exactly one place in the system knows how to turn wiring
   into a monomorphized runner. The alternative — the generator emitting
   forwarder calls, state locals, activation guards and kernel loops itself
   — is a second copy of the emission logic with its own parity burden.
2. **The better front-end for static graphs** (most graphs): same 1.01×,
   zero annotations, implicit captures, in-place error spans, one compile.
   The generator earns its keep only where `graph!` structurally can't go.
   The two partition the space; they don't compete.
3. **`nested()` islands**, independent of how top-level graphs are built.

And the design consumes the next rearchitecture wholesale rather than
obsoleting it:

| Codegen-path need | What provides it |
|---|---|
| something callable to emit calls *to* (no re-implemented semantics) | the `Op` pattern — wall #1's fix; `func!` alone would not have fixed this |
| emission target with types recovered by inference | the forwarder mechanism + activation consts, measured at 1.01× |
| pass 1 (the recorder) | the interpreted `Builder`; side-effect-free wiring makes re-running sound |
| timing/bounds that cannot drift | the shared `Kernel` |
| output format | `graph!` itself |

The genuinely new surface is modest and mostly mechanical: `func!`, the
`OpFn` trait, `EmitLiteral`, `src`/`loc` metadata on builder nodes, and a
walker that prints a wiring fn. The one wasted effort in this story was
already written off — the classic `wingfoil::codegen` retrofit, deleted
because two-pass traversal *without* the Op pattern hits unfixable walls.

## 7. When it earns its keep

The generator is justified only in the intersection of three conditions:

1. topology comes from data (config, DB, discovery) — `graph!` can't
   express it;
2. the graph is hot enough that the ~10× interpreted overhead matters;
3. the config changes no faster than the deploy cadence — frozen captures
   mean regenerate-and-recompile.

Per-instrument pipelines built nightly from a config file sit squarely in
that intersection. Workloads that mutate topology intra-day do not — there,
interpreted mode is the honest answer regardless of this design's
elegance. **A named workload in this intersection is the gate for building
the generator.**

## 8. Sequencing

1. **Now, nearly free:** validate the UX with zero code — hand-write what
   the generator would emit for one realistic config graph (it's just
   `graph!` input) and live with the workflow.
2. **Small, independently valuable, non-breaking:** `func!` + `OpFn` +
   `src`/`loc` metadata. Useful for graph introspection/debugging even if
   the generator is never built.
3. **Gated on the §7 workload:** the generator + `EmitLiteral` + parity
   tests in the house style (run interpreted, run generated-compiled,
   compare values *and* tick times).
4. **Explicitly deferred:** mixed-mode islands (§4.2), capture-as-runtime-
   parameters (§3), multi-module wiring scope.

A further backend behind the same front-end — emitting RustHDL/RHDL and
thence Verilog for FPGA targets — is designed separately in
[`fpga-hdl-backend-decision.md`](fpga-hdl-backend-decision.md); it sits
strictly behind the gates above.

## 9. Known limitations (accepted, documented)

- Implicit environment capture is unavailable to quoted closures — closed
  or explicit-capture only, captures bounded by `EmitLiteral`.
- Frozen captures are partial evaluation: config values are baked at
  generation time (until the runtime-parameters follow-up).
- Body paths must resolve in the artifact's scope: one-module wiring
  convention, crate-rooted paths otherwise; pass-2 errors point into
  generated code (breadcrumb comments mitigate).
- Pass-1 wiring must be deterministic in the generation environment.
- An unquoted closure makes a node generator-ineligible (v1: loud error
  listing call sites; interpreted mode never complains).
