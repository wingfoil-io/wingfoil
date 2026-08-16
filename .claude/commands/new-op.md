Implement a new node/op for **wingfoil** named `$ARGUMENTS`, in the op
catalog (`crates/wingfoil/src/ops.rs` — including statistics ops, whose
fluent trait lives in `src/adapters/statistics.rs`). Follow these steps in
order. Work test-driven: write each
parity test before its implementation.

Ops in wingfoil are **associated functions on a zero-sized witness type**, never
methods on an instantiated object — the semantics are written once and executed
by every engine (interpreted, compiled, nested). The existing ops are the
reference implementations; read them before writing code:

- `src/ops.rs` — the core catalog. `Map` (`#[op(build = map)]`, the smallest
  single-input shape), `Fold` (`init_arg` seeded accumulator), `Ticker` /
  `Const` (sources, with a `start` hook), `Sample` (`passive = [0]`), `Delay`
  (a tick-flag edge), the multi-input `join`/`join3` family and the
  runtime-flag `bimap`/`trimap` methods they back.
- `src/adapters/statistics.rs` — `StatisticsOps`, the template for an op whose
  fluent surface lives in its own feature-gated extension trait outside the
  prelude (EWMA family). The ops themselves stay in `ops.rs`.
- `src/op.rs` — the `Op` trait itself: `Cfg` / `State` / `In<'a>` / `Out` /
  `ACTIVATION`, and `Tick<T>` (`Value` / `Silent` / `Quiet`).
- `src/fluent.rs` — `StreamOps` / `SourceOps`, where the fluent method lives
  (a one-liner over `Stream::wire` / `GraphBuilder::source`).
- `crates/wingfoil-derive/src/lib.rs` — the `#[op]` macro and its flags.
- `docs/adding-an-op.md` — the
  authoritative recipe and the touch-point table; read it first.

## The parity obligation (read first)

Wingfoil's governing design objective (see `README.md` and `CLAUDE.md`) was to
become a **strict superset of the legacy engine**, and that obligation still
binds every op that had a legacy twin. If a legacy node named `$ARGUMENTS`
existed under `legacy/wingfoil/src/nodes/`, it is your **parity oracle**.

> **The legacy tree is deleted; its source is in git history.** It lived under
> `legacy/` until the cutover. To read it, find the deletion commit and look at
> its parent:
>
> ```bash
> DEL=$(git log --format=%H --diff-filter=D -1 -- legacy/CLAUDE.md)
> git show "$DEL^:legacy/wingfoil/src/nodes/<name>.rs"
> git ls-tree -r --name-only "$DEL^" legacy/wingfoil/src/nodes/
> ```

- Read its `MutableNode` impl and its unit tests first.
- Move the legacy `cycle` body **verbatim** into the op — same logic, with
  inputs passed in per cycle (`In<'a>`) instead of read from upstream `Rc`s.
- Every public capability (config knob, mode, tick-suppression rule) needs a
  wingfoil equivalent, or an explicit deviation note in the op docs and, if it's a
  capability gap, in the capability matrix / inventory in `port-plan.md`.
- Port its unit tests as parity tests: identical values **and** tick times.

If no legacy node exists you are defining new surface: keep the naming and
layering conventions below so a future legacy backport stays mechanical.

## Feed lessons back into this skill

Op development keeps surfacing things this skill doesn't yet capture — a
recurring pitfall (the `Fn`-not-`FnMut` closure-config contract; a
`Tick::Silent` vs `Quiet` subtlety), a shape that doesn't fit `#[op]`, a CI
gate you didn't expect, a pattern worth codifying. **When you hit one, bake it
into this file** (`.claude/commands/new-op.md`), ideally in the same PR, or
flag it for a follow-up skill update. This skill is meant to grow with every
op ported — the same way `/new-adapter` grew most of its rules. Record
cross-cutting legacy↔wingfoil differences in `docs/planning/deviation-register.md`.
**Changing an existing op counts too:** if a change invalidates or extends a
rule here, update the rule in the same PR. A skill that has drifted from how we
actually add ops is a bug.

## 1. Branch

**Never edit files directly on `main`** (see `CLAUDE.md`). `main` is the trunk
for every part of this repository — cut the feature branch from it:

```bash
git checkout main && git pull origin main && git checkout -b $ARGUMENTS-op
```

When you open the PR, its **base branch is `main`**.

## 2. Classify the op shape — the load-bearing decision

The shape decides how much you write and which engines you reach for free.
Read the touch-point table in `docs/adding-an-op.md`; the summary:

**`#[op(build = name)]` generates the interpreted `Builder` method for every
shape below** — one `Handle` parameter per edge of `In<'a>`, in `In` order,
then the `Cfg` (omitted when it is `()`), returning the output `Handle`. So the
shape decides what you *write in the op*, not how much wiring you hand-code:

| Shape | `In<'a>` | Attribute | Generated `Builder` signature | Reference |
|---|---|---|---|---|
| **Single-input** | `(&'a I,)` | `#[op(build = name)]` | `name(src, cfg)` | `Map`, `Distinct` |
| **Multi-input, all-active** | `(&'a A, &'a B, …)` | `#[op(build = name)]` | `name(a, b, …, cfg)` | `Join`, `Filter`, `Join3` |
| **Passive edge** (read, don't trigger) | any | `#[op(build = name, passive = [0])]` | same — the mask only changes dispatch | `Sample`, `JoinPassive` |
| **Tick-flag edge** (needs "did it tick?") | `(&'a T, bool)` or `((&'a T, bool), …)` | `#[op(build = name)]` | same — the flag comes from the engine | `Delay`, `Merge2` |
| **Lifecycle hooks** (`start`/`stop`/`teardown`) | any | `#[op(build = name)]` | same — hooks attached automatically | `Ticker`, `Window`, `Timed`, `Finally` |
| **Seeded accumulator** | `(&'a I,)` + init | `#[op(build = name, init_arg)]` | `name(src, init, cfg)` — seeds state *and* slot | `Fold` |
| **Source** (no input) | `()` | `#[op(build = name)]` + `start` that `schedule`s | `name(cfg)` | `Ticker`, `Const` |
| **Signature ≠ shape** | any | `#[op(build = name, no_builder)]` + hand `Builder` method | — | `WithTime` |
| **Phantom type parameter** (a stage, a unit, a marker) | any | `#[op(build = name, explicit = S)]` | `.name::<S>()` — the type crosses as a `PhantomData` argument | `Stamp`, `StampPrecise` |
| **Variadic** (any number of same-type edges) | `&'a [(&'a T, bool)]` | no attribute — hand `Builder` method *and* hand forwarders | `name(&[Handle<T>])` | `MergeN` |

**Declare a tick flag only on edges whose `cycle` actually reads it.** Every
`(&'a T, bool)` pair in `In` costs the interpreted builder one
`ticked_flags()[upstream]` lookup per activation; an edge declared `&'a T` is
handed a constant `false` the op never looks at. A flag destructured as `_` is
therefore pure per-activation overhead, and it also misleads the next reader
about where the op's behaviour comes from. `Filter` had exactly that on its
condition edge: **resampling on a condition tick comes from the edge being
*active*** — the engine activates the node, and `cycle` re-emits the held
source off the condition's current value — never from reading the flag. Active
vs passive is the `passive = [..]` mask; the flag answers a different question
("did *this* edge tick, this cycle?") and only `Delay`, `Merge2`, `MergeN` and
`DelayWithReset` need it.

**`explicit = S` is for a type parameter nothing else mentions.** If an op is
generic over something that appears only in a `PhantomData` — a latency stage,
a unit, a marker — inference cannot reach it from `Cfg`/`In`/`Out`, so the call
site must name it: `.stamp::<quote::produce>()`. Listing it in `explicit` gives
every generated forwarder a leading `PhantomData<S>` parameter, and the `nitro!`
emission passes `PhantomData::<the_arg>` so the type crosses as a **value** and
inference resolves it from an argument.

Do not reach for a turbofish on the forwarder instead — it cannot work. Rust
wants all of a function's type arguments or none, and the macro never learns a
forwarder's arity: it only ever sees a method-name token, which is the whole
point of the naming-convention design. Passing the type as a value is the same
deferral trick `cycle_owned_cfg` uses for a literal closure.

An op declared `explicit` must always be called with a turbofish — omitting one
is a compile error at the call site, which is the intent.

`no_builder` is the last resort, not the default: reach for it only when the
interpreted method must have a *different signature* from the op's shape —
`with_time` seeds its value slot from the input's current value so it never
requires `Out: Default`, and `bimap`/`trimap` take runtime active/passive flags
rather than a compile-time mask (those are extra hand-written methods over
`Join`/`Join3`, alongside the generated `join`/`join_passive`/`join3`). If you
find yourself adding `no_builder` for any other reason, the shape probably fits
and you should check `expand_builder` in the macro crate first.

**Variadic ops** — `MergeN` (the n-ary merge behind `merge_all`/`fan`) and
`CombineN` (`combine`) — are the one shape `#[op]` cannot touch at all: it
parses `In` as a fixed-arity tuple, and a fan-in of runtime width has none. The
route that works, if you need another:

- Declare `In<'a> = &'a [(&'a T, bool)]` — the same uniform `(value, tick)` pairs
  every other op gets, as a slice.
- Hand-write the `Builder` method (`Builder::merge_n`, next to `combine`) and the
  `__wf_op_<name>_*` forwarders + `__WF_OP_<NAME>_{ACTIVATION,PASSIVE}` consts.
  Mirror what `expand_builder` / the forwarder block in the macro crate emit —
  in particular `_start` must be **fully erased** (`<__Cfg, __State>`, no op
  generics) unless the op overrides `start`, because `start` takes no input to
  anchor them from. Getting that wrong fails at the call site with E0282, not in
  the op.
- On the `nitro!` side, set `NodeDef::variadic` where you build the node;
  `cycle_input` then emits a pair *slice* rather than a tuple, and the dispatch
  condition drops the passive mask (a variadic op is all-active, and the mask is
  a `u32` a wide fan-in would shift past).
- **Match the fluent spelling, even if the macro has to learn a new position.**
  A variadic op's arguments are a slice *literal* (the edges must be statically
  visible — a runtime `Vec` has no shape for a compiled graph to emit), so it
  needs its own arm in the macro: `apply_call` for a chained receiver
  (`merge_all`), `walk_chain`'s root arm for a builder-rooted one
  (`push_combine`, the only call there that is not a source). Teaching the
  macro the position was a dozen lines; a second name for one op would have
  been permanent.
- **Do not** make the interpreted path materialise the slice: borrowing all N
  slots per cycle allocates, which on a wide fan-in costs more than the node you
  removed. Factor the semantics into a shared associated fn (`MergeN::winner`)
  that both `Op::cycle` and the interpreted closure call, so they cannot drift.
- **Gate the shape, not just the results.** A variadic op usually replaces a
  chain of binary ones, and the two produce *identical values* — every
  results-parity test passes either way, which is exactly how the merge chain's
  1.86x loss against legacy survived for so long. Assert on
  `Runner::node_count()` that the wiring costs one node (`tests/merge_n.rs`), and
  add a benchmark bar at a width where the difference can show.

**`#[op]` is not in-crate tooling — an out-of-crate op is written exactly the
same way** (#782). The expansion names `::wingfoil::…` throughout and hangs the
generated `Builder` method on a per-op extension trait
(`__WfBuild<CamelName>`, `#[doc(hidden)]`) implemented for
`wingfoil::interp::Builder`, so a downstream author writes `impl Op` +
`#[op(build = name)]` + the three-line fluent method and nothing else. Two
consequences worth knowing before you meet them as errors:

- **The generated method needs its trait in scope**, like any trait method.
  Automatic in the op's own module; `use path::to::__WfBuild<CamelName>;` from
  anywhere else. That is why `fluent.rs`, `signal.rs` and
  `adapters/statistics.rs` glob-import `crate::ops` — **if you add an op whose
  fluent method lives in a file that does not already glob the op's module, add
  the import there**, or you get `no method named <name> found for &mut Builder`
  with a `help:` naming the trait.
- **The dependent must call the crate `wingfoil`.** `::wingfoil::` cannot be
  made `$crate`-relative — a proc macro cannot learn what a downstream crate
  renamed its dependencies to. A renaming crate needs
  `extern crate wf as wingfoil;`. This was already true of `nitro!`.

Worked examples, and the two places to add coverage when you touch the macro:
`tests/custom_op.rs` (an integration test is a separate crate, so every `#[op]`
in it is an out-of-crate expansion) and
`tests/trybuild/pass/out_of_crate_op.rs` (built and run as its own crate in a
throwaway Cargo project — the only place in the repo where `crate::` provably
cannot reach the engine, so it is the test that would catch a regression to
`crate::`-qualified output).

The hand-written route — forwarders by hand, interpreted side through
`register_op1`…`register_op4` / `bimap` / `fold` — is now the **escape hatch**,
not the out-of-crate path. `Ratchet` in `tests/custom_op.rs` is the surviving
example, kept because its state and value slot seed from `Cfg`, which is
neither `#[op]`'s `Default` seed nor `init_arg`'s call-site seed.

The two hard constraints behind this (from `macro-extensibility-decision.md`):
a proc macro sees **tokens, not resolved types**, so `nitro!` can't introspect
an `Op` impl; and a trait **can't be extended from scattered sites**, so the
fluent method's *declaration* is always hand-written (its body usually is not —
see step 4). Everything else is generated.

**Scheduling / activation.** Set `const ACTIVATION` from behaviour, not habit:

- `Activation::NONE` — pure transform, ticks only when an active input ticks
  (`map`, `fold`).
- `Activation::SCHEDULES` — time-gated / self-scheduling; call `ctx.schedule(t)`
  (`ticker`, `delay`, `throttle`).
- `Activation::ALWAYS` — runs every cycle (`always`).
- `Activation::THREADED` — fed from a background thread / external waker
  (channel / external sources — adapter territory, see `/new-adapter`).

**`Tick` variant** is a correctness contract, not a style choice:
`Tick::Value(v)` ticks downstream; `Tick::Silent(v)` updates the value slot
**without** ticking (what `delay` needs so a passive reader never sees
`T::default()`); `Tick::Quiet` emits nothing (warm-up, suppressed duplicate).

**`ctx.is_last_cycle()` only fires if the op is *cycled* on that cycle.** An op
that flushes on the final cycle (`buffer`, `window`, the Python `dataframe`
binding) but is `Activation::NONE` is only cycled when an active input ticks —
so a stream that has gone quiet before the run ends (a slower ticker, anything
behind `limit`) never reaches the flush and its value stays at the default. That
is a real, observable outcome, not a theoretical one: `dataframe()` on the slow
half of two tickers at different rates yields Python `None`. If the flush must
happen regardless, the op needs `Activation::ALWAYS`; if not, say so in the op
docs and name the alternative (`collect` for `dataframe`) so callers can pick.

## 3. Implement the `Op`

In `ops.rs`, add a zero-sized witness type and its `impl Op`:

```rust
/// <one line: what it computes, and the tick-suppression rule>. <If porting:
/// "Ports legacy `wingfoil::nodes::$ARGUMENTS`.">
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
        // legacy cycle body, verbatim; inputs passed in, not read from Rc<dyn Stream>
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
  A seed that must come from the call site is the `init_arg` shape (`Fold`),
  which `#[op]` also generates — not a reason to hand-write a builder.
- **Convert config once in `start`**, not per `cycle`. `Ticker` converts its
  `Duration` period to `NanoTime` in `start` and `TickerState` documents the
  pattern; `Throttle`, `Delay`, `DelayWithReset` and `Window` follow it. One
  multiply-and-add per cycle is not what this is about — it is that a
  `Cfg`-derived value has exactly one place it is computed, so `cycle` reads a
  field and cannot disagree with `start` about what the config meant.
- **Overriding `start` obliges every op generic to appear in `Cfg` or
  `State`.** This is the trap that comes with the bullet above, and it bites
  at the *call site*, not in your op. When an impl overrides `start`, `#[op]`
  emits a **real** `__wf_op_<name>_start` forwarder carrying the op's
  generics — and `nitro!`/`compiled` call it with nothing but the `Cfg` and
  `State` values to infer from (`start` takes no input). A generic that
  appears in neither dangles: `error[E0282]: type annotations needed ... on
  the function __wf_op_<name>_start`, pointing at the user's `.my_op(..)`
  call. When the op does *not* override `start` the forwarder is a fully-erased
  no-op and the question never arises — so an op can acquire this failure just
  by gaining a `start` hook.

  Anchor the generic in the `State` type, with `PhantomData` if the state has
  no real use for it. `ThrottleState<T>` is the minimal worked example (a
  throttle stores no values, so `T` is carried purely as the anchor);
  `DelayState<T>` / `WindowState<T>` anchor theirs through real payload and
  needed no change.
- **Validate user config** at wiring when possible; when it needs runtime info,
  `anyhow::bail!` inside `cycle` with a clear message (aborts the run). **Never
  panic** for bad user config. No `.unwrap()` outside `#[cfg(test)]` / doc
  examples (repo-wide rule).
- **Doc every public item.** `#[op]`-scoped ops get a `Builder::$ARGUMENTS`
  method whose docs are the witness type's docs — write them for a caller.
- **Prefer a reference bound on an input the op only reads.** An op whose input
  is a container (`Burst<T>`, `Vec<T>`) and whose `cycle` needs one item out of
  it should bound `for<'b> &'b T: IntoIterator<Item = &'b OUT>` and iterate the
  borrow — not `T: Clone + IntoIterator<Item = OUT>`, which clones the whole
  container and discards all but what it emits. That costs an allocation plus a
  clone per item, and it lands **exactly under producer load**: bursts are
  single-item until a producer outruns the cycle, so a quiet test never shows
  it. `Collapse` is the worked example (#824).

  **A bound on an op's inputs threads itself — the fluent *declaration* does
  not.** `#[op]` copies the impl's whole `where` clause into one shared
  predicate list used by every generated surface (the `nitro!` forwarders, the
  `Builder` method, `__wf_fluent_<name>!`, `__wf_signal_<name>!`); HRTBs and
  associated-type bindings survive verbatim, and the receiver's ident is
  rewritten to the macro's `$t` inside them. So there is nothing to change in
  `wingfoil-derive`. What you **must** update in the same commit is the
  hand-written trait declaration in `fluent.rs`, because rustc checks the
  generated body against it — a stale bound there fails with *"the requirement
  `T: X` appears on the `impl`'s method but not on the corresponding trait's
  method … originates in the macro `__wf_fluent_<name>`"*. That error is the
  macro working; do not widen the generator to silence it.

  Cover such a change on **all three tiers** and for every container shape the
  op claims to accept. `<&Burst<T> as IntoIterator>::Item` is `&T` via tinyvec's
  slice iterator, same as `Vec<T>` — but confirm rather than assume, and note
  that `&Burst<T>: IntoIterator` implies `T: Default` (tinyvec's `Array`
  bound). Where the point of the change is *how much* work an op does, pin it
  with a payload whose `Clone` increments a counter, and sanity-check the
  assertion by making the op clone per item and watching it fail.

## 4. The fluent method — declaration by hand, body usually generated

`#[op]` cannot add a method to a trait (constraint #2), but it can *write* one:
`#[op(build = $ARGUMENTS, fluent)]` also emits `__wf_fluent_$ARGUMENTS!`, a
`macro_rules!` the trait's `impl` block invokes. So the split is:

- **Declaration — always by hand.** It is the documented public surface, and
  rustc checks the generated body against it, so a signature that drifts from
  the op's shape is a compile error.
- **Impl — the macro invocation** in the trait's `impl` block, in place of the
  body.

The generated signature is `(&self, <edges 1..>, <init>, <cfg>)` — edge 0
becomes `&self`, later edges become `&Stream<_>` params, then an `init_arg`
seed, then the config. **Declare it in that order** or the two will not match.

**The op's `In` decides the receiver, and the receiver decides how you invoke
the macro** — you do not choose. There are three shapes, all generated
(`tests/op_fluent_shapes.rs` pins each from a trait defined outside the crate):

| Edge 0 of `In` | Receiver | Invocation | Trait it lands on |
|---|---|---|---|
| a bare type parameter | `Stream<T>` | `__wf_fluent_$ARGUMENTS!(T);` | `StreamOps`, an adapter trait generic in its payload |
| a concrete type (`&f64`) | that fixed stream | `__wf_fluent_$ARGUMENTS!();` | `StatisticsOps` and other domain traits |
| no edges at all (a source) | `GraphBuilder` | `__wf_fluent_$ARGUMENTS!();` | `SourceOps` |

A source's body wires through `GraphBuilder::source` rather than
`Stream::wire`; nothing else differs. Its own type parameters (`constant`'s
payload) stay method generics.

**The invoking file needs the op's `__WfBuild<Name>` trait in scope**, because
the generated body calls the generated `Builder` method. In-tree that is what
the `use crate::ops::*;` glob in `fluent.rs` / `signal.rs` /
`adapters/statistics.rs` (and `use wingfoil::ops::*;` in
`tests/op_fluent_shapes.rs`) is for. Symptom if you miss it: `no method named
<name> found for &mut Builder`, with a `help:` naming the trait.

There is a **fourth shape, and it is rejected**: an edge 0 that *mentions* a
type parameter without being one — `Burst<T>`, `Vec<T>`. It is not generic
(no single ident for the invoking impl to bind) and not concrete (the impl
still has to supply something), so `#[op(fluent)]` errors on the type telling
you to make it one or the other, or drop `fluent`. Do not read that error as a
gap to widen the macro through: a receiver of `Stream<Burst<T>>` almost always
means the *payload* is the parameter, so the fix is usually the op's `In`, not
the generator.

Write the body by hand instead when any of these hold — all three are real,
current cases, not hypotheticals:

- the op is `no_builder` (nothing to forward to) — `with_time`;
- the fluent signature deliberately reorders the parameters —
  `delay_with_reset(delay, trigger)` puts the cfg before the edge;
- the fluent signature's types differ from the op's `Cfg` — `logged` (see the
  gotcha below). **Prefer changing the `Cfg` to fixing it here:** the
  `time_windowed_*` family used to be the other example in this bullet, and
  closing it (taking `Duration` as the `Cfg`, converting once in `start`) both
  deleted 11 hand-written methods and put the family in `nitro!`;
- the body does more than forward — `ewma_per_tick` `debug_assert!`s its alpha
  is in `[0, 1]` before wiring.

Hand-written, it is a one-liner over `Stream::wire`:
  ```rust
  fn $ARGUMENTS<B, F>(&self, /* args */) -> Stream<B>
  where B: Clone + Default + 'static, /* … */
  { self.wire(|b, h| b.$ARGUMENTS(h, /* args */)) }
  ```
- Multi-input → `self.wire(|b, h| b.$ARGUMENTS(h, other, /* … */))` over
  `register_op2` (see `join` / `bimap`).
- Statistics / domain op → its own extension trait kept **out of the prelude**
  (`StatisticsOps` in `adapters/statistics.rs`); users opt in with
  `use wingfoil::adapters::statistics::StatisticsOps;`, mirroring adapters. The trait is
  yours to declare; only the body comes from the macro.

## 4a. Four API shapes worth getting right the first time

These came out of auditing `latency.rs` — the whole module had all four wrong,
and every one of them is cheap to fix on the day and expensive later, because
each is a *breaking* change once callers exist.

**1. A knob that varies at runtime is an argument, not a method name.** If an
op has two variants and an on/off, do not ship `foo` / `foo_precise` /
`foo_if` / `foo_precise_if`. That is four methods that still cannot express
"variant chosen by a config flag" — the only spelling is a pair of `_if` calls
with opposite polarities, which silently applies the op twice the moment one
`!` is dropped. Ship one method taking a small `Copy` enum (`Stamping`), with
constructors for the shapes a config has (`Stamping::new(enabled, precise)`),
and keep the named forms as documented shorthands. Note that a mode-taking
method is fluent-only by construction — it picks *which node to insert*, a
wiring-time branch, where an op only ever describes a cycle — so it gets no
`nitro!` forwarder and belongs in the fluent-only list in
`tests/op_completeness.rs`.

**2. If the op is usually chained with itself, offer a fused form.** Every op
clones its input to produce its output — each node owns its output slot, so
that is the engine's model, not something an op can opt out of. An op that
writes 8 bytes therefore pays a full payload clone, and on a `Stream<Burst<T>>`
that clone is a `Vec` allocation. When N of them typically sit adjacent, a
tuple-taking form (`stamp_all::<(A, B)>(..)`) does the work of N nodes with one
clone. Make it *exactly* equivalent, not an approximation: if the single form
reads a clock per node, the fused form reads a clock per element of the tuple.
The trait it dispatches on cannot be blanket-implemented for the single case —
`impl<S: Stage<L>> StageSet<L> for S` collides with the tuple impls under
coherence, because a downstream crate could implement `Stage` for a tuple — so
implement it for 1- to 8-tuples and let the single-stage method stand alone.

**3. An observation that cannot be measured is tallied, never recorded as
zero.** Any op that folds deltas, ratios or intervals into a statistic will
meet inputs from which no number can be derived. Recording a 0 for those is the
worst option available: it is indistinguishable from a real measurement at the
bottom of the range, and it drags `min`, `mean` and every low quantile with it.
Count them in a named field per reason (`same_instant` / `backwards` /
`unstamped`), and make the *renderer* print a dash and the reason rather than a
number. Silently skipping them is nearly as bad — a `count` that quietly
excludes a third of the input still reads as a complete measurement.

**4. A handle you hand back is a newtype, not `Rc<RefCell<T>>`.** Returning the
sharing mechanism pins it into the signature and gives the caller nothing but
`borrow()`. A newtype can carry the operations the raw cell cannot: `reset` and
`take` (without them a cumulative statistic never recovers from one outlier —
it is a record, not a reading), a small `Copy` read-out type so consumers stop
indexing internals, and a `windows(&g, period)` method that turns the
accumulator into a `Stream`, which is what any real consumer wants — a
teardown `print!` is not an output edge. Ship the shapes a caller in the tree
actually reaches for and no more: the same review that added those also had to
delete a cumulative `snapshots()` twin, a `with(|stats| ..)` beside `borrow()`,
and `merge` at three levels, none of which ever acquired a caller.

## 4b. The `Signal` facade — one line, and don't skip it

`#[op(build = $ARGUMENTS, fluent)]` emits a **second** macro,
`__wf_signal_$ARGUMENTS!`, which writes the same combinator for the
builder-less [`Signal`] facade in `src/signal.rs`. Add the one line to the
generated `impl<T: 'static> Signal<T>` block:

```rust
__wf_signal_$ARGUMENTS!(T);      // or !(); for a concrete receiver
```

**This is a real obligation, not a nicety.** The facade's forwarding was
hand-written until 2026-08, and it silently fell 15 methods behind the catalog
— `logged`, the whole `join`/`try_join` family, `drop_small_change` — because
each op author had no reason to think of it. One line now, or the same drift
again.

The macro is skipped in exactly two cases, both mirroring step 4: a **source**
(it enters the facade as a free function that *makes* the graph — hand-write it
next to `ticker`/`constant`), and an op whose `Signal` signature is not a plain
forward (`logged`'s `&str` label vs its `(String, Level)` `Cfg`). Put those in
one of the bound-grouped blocks lower in the file with a comment saying which
case applies.

Note the generated body wires through `Stream::wire` and the op's `Builder`
method — **not** through the fluent method. A hand-written trait declaration
may legitimately be stricter than the op needs (`StreamOps::accumulate`
requires `T: Default` where the op, whose `Out` is `Vec<T>`, does not), and
going through it would import that bound into a signature nobody wrote.

## 5. `nitro!` / compiled coverage

For any `#[op]` op this is **zero-touch**: the attribute emits the
forwarder functions (`__wf_op_<name>_cycle`, `__WF_OP_<NAME>_ACTIVATION`) that
compiled/nested emission dispatches through by naming convention, and rustc's
inference resolves the op type the macro never names. Nothing to edit.

For a shape `#[op]` cannot parse (cyclic/IO sources; an `In` the uniform
one-`(value, tick)`-pair-per-edge form can't express, as
`DelayWithReset` — see its `DelayWithResetFwd` witness for the way out;
variadic, as `MergeN` — see step 2 for its route), the op
may land **interpreted-only** —
that is allowed, but it must be *consciously* placed: either give it an
equivalent forwarder, or add it to the documented fluent-only allowlist in
`tests/op_completeness.rs` (see step 6). Never leave it silently in neither.

**Gotcha — an ergonomic fluent signature that differs from the op's `Cfg`
forces fluent-only.** `nitro!`/compiled emission uses the **call-site argument
types verbatim** as the op's `Cfg` (a plain arg → `__cfg` local, tuple in call
order), then hands them to `__wf_op_<name>_cycle(__cfg: &mut <Cfg>)`. So a
call-site type must *equal* the `Cfg` type. If the fluent method takes a
different, more ergonomic type and converts — the legacy pattern being a
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

Mirror the legacy node's own unit tests. Conventions (see `tests/catalog.rs`,
`tests/catalog_ops.rs`, `tests/catalog_flow.rs`):

- Run historical for determinism: `RunMode::HistoricalFrom(NanoTime::ZERO)`.
- Assert **values and tick times** — build the graph, `r.run(...)`, and check
  `r.value(&stream)`; use `.with_time()` / `.accumulate()` to capture tick
  timing, not just the final value. Tick **suppression** (an op that goes
  `Quiet`) is part of the contract — assert the suppressed ticks are absent.
- Port every legacy unit test first, then add wingfoil-specific cases.

### Completeness / engine-parity guard — `tests/op_completeness.rs`

This is a **compile-time** guard against one-sided registration: a combinator
used inside a `nitro!` block only compiles if it has **both** a fluent method
**and** a forwarder. So:

- **Dual-mode op** → add it to a `nitro!` block here; each block also asserts
  `interpreted() == compiled()`, extending engine-parity across your op.
- **Deliberately fluent-only op** (IO/cyclic source, or a not-yet-forwarded
  shape) → add it to the documented allowlist in this file with a one-line
  reason. Adding an op means consciously choosing one of these — never
  silently neither.

**Check the guard actually covers your op's trait.** The rule above is only
worth what its coverage is, and it has been wrong at trait granularity: the
whole of `StatisticsOps` — 36 methods — sat in neither list for its entire
life, because the file was written against `StreamOps` and nobody widened it
when the statistics surface landed. If you are adding the *first* op to a new
fluent trait, the block exercising it does not exist yet; write it, or the
next 35 methods inherit the hole. A cheap check: every `#[op]` in the catalog
should be reachable from a `nitro!` block in this file or named in one of its
categories.

If your op lands in **2b** (fluent signature ≠ the op's `Cfg`), say which of
the two it is: a deliberate ergonomic split that costs nothing compiled
(`logged` — a debug tap has no place in a compiled kernel), or a real gap
worth closing (a compiled kernel *should* carry the op). **Default to closing
it.** The fix is to take the ergonomic type as `Cfg` and convert in `start`,
the way `Ticker` does — never to touch the macro. The `time_windowed_*` family
was 2b's worked example of the second kind and is now the worked example of
the fix: `Cfg` became the `Duration` the fluent methods already took, the
converted window moved into a `TimeWindowed<S>` state wrapper (the accumulators
are shared with the count-windowed ops, which have no window to hold), and all
11 methods became generated. If the state you would convert into is shared with
another family, wrap it — do not add a field only one family reads.

Compiled-specific stateful/lifecycle behaviour has its own suites
(`tests/compiled_stateful_ops.rs`, `tests/compiled_lifecycle_ops.rs`,
`tests/nested_islands.rs`) — extend the relevant one if your op is stateful or
has `start`/`stop`/`teardown` hooks.

**A *delegating forwarder* must delegate `start` too, and cross-engine parity
cannot tell you when it doesn't.** An op whose `In` shape `#[op]` cannot parse
is restated by a forwarder witness that carries the attribute and delegates to
the real op (`DelayWithResetFwd` → `DelayWithReset`). Every tier — interpreted
included — reaches the op *through* that witness, so a hook the witness forgets
to forward is missing from all of them **identically**, and an
`interpreted() == compiled() == nested()` assertion passes on the shared wrong
answer. Dropping `DelayWithResetFwd::start` turns every `delay_with_reset` in
the tree into a pass-through and all three tiers agree that it should. Pin such
an op with an **absolute** expected sequence (values *and* tick times), not
only against another tier, and sanity-check the test by deleting the delegation
and watching it fail.

**A lifecycle hook is only covered when its *effect* is observed on all three
tiers.** `#[op]` emits the `_start` / `_stop` / `_teardown` forwarders for every
op, and `nitro!` calls one per node — but a test that merely runs the graph and
checks the output value passes whether or not the hook ran. That is how the
`stop`/`teardown` emission stayed missing from `compiled()` / `nested()` long
after the forwarders existed (#783), and how the `stop` half then stayed
unguarded after it landed: the only catalog op with a real `stop` is `timed`,
whose hook *prints*. So for an op with an end-of-run hook, add a case to
`tests/compiled_lifecycle_ops.rs` that:

- records the hook firing into a thread-local log (a counter for "exactly
  once", an ordered `Vec` for ordering), and asserts the log after
  `interpreted()`, `compiled()` **and** `nested()`;
- records the op's **state** as the hook saw it, not just that it fired — that
  is what pins the hook against the node's real accumulated state rather than a
  fresh seed;
- pins the ordering the interpreted `Runner` defines and the other two must
  mirror: every node's `stop` in node order, then every node's `teardown` in
  node order, with cleanup running even after a `start`/`cycle` abort (first
  error wins). `finally` is the ready-made teardown probe; a `stop` probe has to
  be a purpose-built op.
- **Sanity-check the guard by breaking the thing it guards.** Delete the
  emission (the `#(#stops)*` / `#(#teardowns)*` splice in `expand_compiled` /
  `expand_nested`), confirm your test — and ideally *only* your test — fails,
  then restore. A lifecycle test that passes both ways is worth nothing.

For a probe op keep the `Cfg` equal to the call-site argument type (a
`&'static str` label, not an owned `String`): the compiled emission uses
call-site types verbatim, so an "ergonomic" config makes the op fluent-only and
it never reaches the tiers you are trying to test (step 5's gotcha).

### Python bindings — see step 7

## 7. Python bindings (`wingfoil-python`)

**This step is part of adding an op, not an optional extra** — see the end of
the step for when skipping is legitimate and what to write in the PR when it
is. Decide it before you start the work, not after.

`wingfoil-python` is the **go-forward** Python binding (it supersedes
legacy `wingfoil-python`; see `docs/python-interop.md`). Everything
Python-composable rides one erased edge type, `PyElement` — **only the edges
erase**, the op interior stays natively typed. pyo3 forbids `#[pymethods]` on a
foreign pyclass, so a user op becomes a **free `#[pyfunction]`**
(`module.$ARGUMENTS(stream, …)`), not `stream.$ARGUMENTS(…)` — the same shape
polars expression plugins use.

Pick the lightest tool that fits:

- **Stateless single-input** (with or without one config arg) → the
  `pyop_fn!` declarative macro (`crates/wingfoil-python/src/macros.rs`):
  ```rust
  pyop_fn! {
      /// <doc>
      fn $ARGUMENTS(cfg: f64): f64 => f64 = |cfg, _state, a, _ctx| Ok(Tick::Value(/* … */))
  }
  ```
- **Any concrete one-, two- or three-input op** → the `#[pyop]` **proc**
  macro (`wingfoil-python-derive`), placed alongside `#[op]` on the `Op`
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
  `Builder::register_op<n>` (mirror `register_op4`: grab the N `SlotRef`s and
  hand `register_op_cell` a closure that borrows them and calls `step` — a
  dozen lines, since the registration shape itself lives in that shared core),
  `PyStream::wire_op<n>`, and the parameter name
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
   discipline as everywhere in wingfoil.
4. **pytest** in `tests/test_interop.py` — call `wingfoil.$ARGUMENTS(...)`,
   compose it between built-in combinators, and assert the result. Include a
   round-trip that also authors the same graph purely in Rust and asserts they
   agree, when practical (the parity-oracle discipline).

**Bind the op in the same PR as the op, and treat that as the default.** The
binding is small once the `Op` exists — for a plain-`Cfg` op it is the
`#[pyop]` line plus registration, and for a fluent forward it is six lines —
and an op that lands without one just becomes a second PR someone has to
remember. `/new-adapter` already says this for adapters; it holds at least as
strongly here, because an op is the smaller unit and the binding is
correspondingly cheaper.

The bar for *skipping* is a reason the binding cannot be a plain forward, not
merely the absence of a precedent — and that bar is lower than it looks. Even
`logged`, a debug tap deliberately kept fluent-only on the Rust side, and
`accumulate`, a *test instrument* that has no business in a compiled kernel,
are both bound (`graph.rs:480`, `graph.rs:486`). If those clear it, a normal
combinator does. Reasons that do hold, all of which still get a sentence in the
PR description:

- the op's `Cfg` is a Rust closure, so `#[pyop]` / `pyop_fn!` cannot reach it
  and the binding is hand-written work (see the gotcha below) — a legitimate
  "not in this PR";
- the op's *shape* needs a hand-written method rather than a macro line — a
  passive edge (`join_passive`, unbound today for exactly this reason) or an
  arity of 5+, which needs a `register_op<n>` primitive first;
- Python already spells the same thing under another name — the way Python's
  `fold` *is* `scan`, its callable returning the accumulator because there is
  no `&mut`.

**"No legacy twin" is not on that list, and it is the one to watch for.** It
sounds like parity reasoning and is actually its inverse: the parity obligation
is a *floor* on what wingfoil must expose, never a ceiling, so deriving the
Python surface from what legacy happened to bind freezes it as a snapshot of
the old engine rather than of the current catalog. New surface is exactly the
surface with no legacy twin.

The cheap check that catches this: **look at the op's nearest mirror.** If a
sibling op is bound, bind this one — a Python user who has `limit` and reaches
for `skip` should find it. `skip` (#846) is the worked example of getting this
wrong: it was written with no binding on the stated grounds that it had no
legacy Python twin, while `limit` — the op it mirrors, six lines of plain
forwarding in `graph.rs` / `python.rs` — had been bound all along. It now has
those six lines, added in review before it ever reached `main`; the rule earned
its place by catching this one, so apply it to yours before the reviewer does.

If you do skip, say so explicitly in the PR description with which of the
reasons above applies, so reviewers can weigh the call instead of re-deriving
it.

**Gotcha — a closure `Cfg` cannot be bound by reusing the op.** Neither
`#[pyop]` nor `pyop_fn!` helps when the op's `Cfg` is a caller-supplied Rust
closure (`Map`, `MapFilter`, `DropSmallChange`): the op's bound is `Fn(..) ->
T`, *infallible*, and a Python callable can raise. So the binding wires
`Builder::register_op1` (or `register_op2`…) directly, with the Python callable
as the cfg and the op's `State` shape restated, converting a raised exception
into an `anyhow` error that aborts the run — the shape `PyStream::{map, fold,
filter_value, filter_map}` already use. Keep the op and the binding's `cycle`
bodies visibly the same so they cannot drift, and say in the binding's doc why
it does not go through the op.

**The legacy binding is a parity oracle too, not just the legacy node.** If the
legacy `wingfoil-python/src/py_stream.rs` (in git history, as above) exposed the
op, its Python-level contract is part of what wingfoil must be a superset of —
including how strictly it validates the callable's return. `drop_small_change` extracts a strict `bool`
(and errors with "must return a bool") rather than following the `is_truthy`
convention its neighbours in `graph.rs` use, precisely because the legacy
binding does and has a test pinning it. Port those binding tests alongside the
node's.

**…and the legacy oracle is not only Rust.** Legacy surface also lived in the
pure-Python package at `legacy/wingfoil-python/python/wingfoil/` (e.g.
`pandas_helpers.build_dataframe`, re-exported from its `__init__.py`). A grep of
`py_stream.rs` misses those entirely, so check the Python package and its
`__all__` too when establishing what a binding must cover.

**Not every Python surface is an op.** Some legacy vocabulary is a *post-run
helper* over already-run streams rather than a node — `build_dataframe`
outer-joins several streams' recorded histories after the run and touches no
graph. Do not force it through `#[pyop]` / `pyop_fn!`: write it as a plain free
`#[pyfunction]` that reads `Stream::value()` and register it in the
`#[pymodule]` like any other. Two conventions still apply, and they are what
makes it feel native rather than bolted on:

- **Put the logic in `graph.rs` and the glue in `python.rs`,** the same split
  the pyclass methods use — `graph.rs` owns the erased object form and the real
  work (returning `anyhow::Result`), `python.rs` owns `#[pyclass]`/`#[pyfunction]`
  argument extraction and the `to_pyerr` mapping.
- **Prefer Rust over a new pure-Python module.** `python/wingfoil/` exists
  only for what genuinely needs Python (subclassing, in `CustomStream`), and its
  `__init__.py` re-export is *derived* from the extension — so a Rust
  `#[pyfunction]` appears in `wingfoil.*` with nothing to keep in sync,
  while a Python helper is a second hand-maintained surface. Even helpers that
  are "just pandas calls" belong in Rust for that reason.

A post-run helper has no `nitro!`/compiled obligation (steps 5–6 do not apply),
but it still owes a Rust unit test beside the other `graph.rs` tests and a
pytest.

**A *family* of ops binds as one dispatcher, not one function per op.** The
op-per-binding assumption above breaks whenever the legacy Python contract
parameterises a whole family with argument *objects*. The statistics surface is
the worked example (`crates/wingfoil-python/src/statistics.rs`): the engine
spells out ~40 methods on `StatisticsOps` (`rolling_mean`,
`time_windowed_mean`, `cumulative_mean_time_weighted`, …) because Rust affords a
wide statically-checked surface; legacy Python offered eight methods times two
orthogonal knobs (`Window`, `Weighting`). Neither `#[pyop]` nor `pyop_fn!` can
express that — **which op to wire is runtime data**, decided after the arguments
are resolved — so the binding is a hand-written dispatcher over
`Stream::wire`. Rules that fell out of it:

- **The binding surface is the legacy binding's, not the engine's.** Do not
  expose one Python function per engine method just because they exist; the
  parity target is legacy's method list and its `int`/`str`/`float` shorthands.
  Exhaustive `match` on the resolved knobs is the guard — a new engine
  combination becomes a compile error in the dispatcher.
- **A `#[pyclass]` used as an *argument* needs `from_py_object`.** On pyo3 0.29,
  `#[pyclass(eq, frozen)]` alone leaves the `FromPyObject` derive deprecated, so
  `arg.extract::<PyWindow>()` warns (and will stop compiling). Spell it
  `#[pyclass(name = "Window", frozen, eq, from_py_object)]`, and register the
  class in the `#[pymodule]` next to `Graph`/`Stream`.
- **Validate at the boundary what the op only `debug_assert!`s.** `Ewma`
  debug-asserts `alpha ∈ [0, 1]`; a release wheel would otherwise return a
  silently diverging average, so `EwmaSpan.per_tick` raises `ValueError`. Any
  `debug_assert!` in an op is a Python-visible hole until the binding closes it.
- **Give a typed-edge family one labelled conversion seam.** Ops on
  `Stream<f64>` need `PyElement → f64 → PyElement` at both ends;
  `PyStream::wire_float_stat(op, |s| …)` does it once and names `op` in the
  error, so a non-numeric value reports *which* operator demanded a number
  (legacy's `as_floats` contract). Don't repeat `try_map(f64::try_from)` per
  method.
- **Reject an ambiguous shorthand rather than guessing.** `mean(10)` (ten
  samples) and `mean(10.0)` (ten seconds?) cannot both be right, so a bare
  `float` window is a `TypeError` pointing at `Window.seconds(...)`. Note that
  `extract::<usize>` goes through `__index__`, so a `float` falls through to
  that error instead of being silently truncated — the ordering of the
  `extract` attempts is load-bearing.

## 8. Roadmap bookkeeping

Update `docs/planning/port-plan.md`: mark `$ARGUMENTS` in the Phase 2 inventory
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
cargo test -p wingfoil                  # catalog + completeness + parity
# if you touched Python bindings:
cargo test -p wingfoil-python           # the Rust seam tests
cd crates/wingfoil-python && maturin develop && pytest
```

All must pass before committing. `cargo lint-all` is what CI runs — it is the
only lint pass that sees feature-gated code (e.g. a `statistics`/`augurs` op).

**Sandbox caveat** (same as the adapter skill): `cargo lint-all` is a workspace
all-features build, so it also compiles the legacy **aeron** C library, which
fails in a dev sandbox without the native toolchain — unrelated to your change.
When that blocks you, run the scoped equivalent that still lints every
`wingfoil` feature/target:

```bash
cargo clippy -p wingfoil --all-features --all-targets -- -D warnings
```

Note the substitution in the PR; the full workspace `lint-all` runs in CI.

## 10. Self-review with a fresh context

Before opening a PR, run a clean-context review pass as a subagent:

1. **Re-read this skill end to end**, then walk `git diff main...HEAD` against
   steps 1–9 and produce a present / missing / diverged checklist. Flag every
   divergence, even intentional ones.
2. **Validate the artifacts**: branch cut from `main`, PR base `main` (step 1);
   the `Op` impl with correct `ACTIVATION` and `Tick` variants (steps 2–3);
   closure configs are `Fn` not `FnMut`; `State: Default` (or `init_arg`); a
   `no_builder` is justified by a signature that differs from the shape, not by
   the shape itself; a fluent method on the right trait — declaration by hand,
   body via `__wf_fluent_<name>!(T)` unless step 4 says otherwise — out of the
   prelude for a domain op (step 4); `nitro!`/compiled coverage is zero-touch or
   the op is a documented fluent-only entry (step 5); catalog tests assert
   values **and** tick times and the op appears in `op_completeness.rs` (a
   `nitro!` block or the allowlist) (step 6); Python binding + registration +
   seam test + pytest — the default, in this PR — or one of step 7's listed
   reasons it cannot be a plain forward, stated in the PR description ("no
   legacy twin" is not one of them; check the op's nearest mirror) (step 7);
   port-plan updated (step 8).
3. **Check parity**: diff against the legacy node — every legacy test has a
   wingfoil twin with identical values and tick times; the deviations list in the
   op docs is complete.
4. **Run the pre-commit checklist from step 9** and confirm every command
   passes. Do not skip any.
5. **Review for quality**: no speculative abstractions, no dead code, no
   comments restating the code, no half-finished paths.

Fix everything found before committing. A clean self-review is part of "done".
