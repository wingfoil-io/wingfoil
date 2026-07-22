# Phase-1 contract proposals

Status: **one open proposal remaining.** This document originally captured three
Phase-1 contract-shape decisions (`docs/port-plan.md`, "Phase 1 — contract
completion") for a human to ratify *before* any of them landed, because each one
touches the `Op` contract, all three engines, and the macro at once — the exact
blast radius the plan front-loaded fallibility (spike 0.1) to avoid retrofitting
later. Two have since resolved on `next`: **§1 `Tick::Silent(T)` is implemented**
(`wingfoil-next/src/op.rs:85-89`, via #496) and **§3 `supported_ops!()` is
superseded** (#496 deleted the macro op-table). Only **§2, the per-node
`reset`/`setup` hook, is still an open decision.**

The three items — of which **only item 2 is still open**; items 1 and 3 have since
landed / been made moot on `next`:

1. ~~**`Tick::Silent(T)`**~~ — **implemented on `next`** (see §1). The
   `Tick::Silent(T)` variant landed in `wingfoil-next/src/op.rs:85-89` (via #496 /
   the same line of work that deleted the op-table), and `delay`'s
   store-without-tick semantics are now expressed through it generically across all
   engines. No longer an open decision.
2. **A per-node `reset`/`setup` hook** — the missing lifecycle hook that makes a
   `Runner` re-runnable, which the compat facade (and the wingfoil-python gate)
   requires. **This is the one remaining live proposal.**
3. ~~**A `supported_ops!()` completeness test**~~ — **superseded by #496** (see
   §3). #496 deleted the macro's op-table, so there is no longer a macro op-list
   that can drift from the fluent surface; the completeness test it would have
   guarded is no longer needed.

Each section states the problem with `file:line` evidence, lays out the options,
and gives a recommended design (types, blast radius, migration, tests). A
ratification checklist and recommended landing order close the document.

---

## 1. `Tick::Silent(T)` — update the value slot without ticking — implemented

**Status: implemented on `next` (`wingfoil-next/src/op.rs:85-89`) — superseded. No
action needed.**

The recommendation below (option (a): add `Tick::Silent(T)` to the contract) was
adopted. `wingfoil-next/src/op.rs` now defines
`enum Tick<T> { Value(T), Silent(T), Quiet }`, with the `Silent(T)` variant
documented as "update the value slot, but do not tick downstream." It landed via
#496 / the same line of work that deleted the macro op-table (§3), and `delay`'s
store-without-tick and zero-delay semantics are now expressed in `Delay::cycle` and
handled generically by every engine — retiring the name-keyed `delay` special case
this section set out to remove. See the architecture doc (PR #494) for the shipped
design. Only the `reset`/`setup` hook (§2) remains an open decision.

<details>
<summary>Original recommendation (retained for history — implemented on `next`)</summary>

### The problem, precisely

`Tick<T>` has exactly two variants (`wingfoil-next/src/op.rs:89-93`):

```rust
pub enum Tick<T> {
    Value(T),   // ticked, here is the value
    Quiet,      // did not tick
}
```

There is no way for an op to say "store this as my current value, but do **not**
propagate a tick to my downstreams." Classic `delay` needs exactly that: it
stores the *first* upstream value into its slot without ticking, so a passive
reader (a `sample`/`join`/passive edge on the delayed stream) sees the real first
value instead of `T::default()` before the delay elapses
(`docs/fable-review.md:120-127`, bug 3).

Because the contract can't express it, the behaviour lives at the **engine**
level, keyed on the op's *name*, in two places that must be kept in lockstep:

- **Interpreted** — `Builder::delay` carries a bespoke closure with an extra
  `seeded` flag alongside the op's own state
  (`wingfoil-next/src/interp.rs:1057-1114`). The store-without-tick is
  `interp.rs:1096-1101`:

  ```rust
  Tick::Quiet => {
      if src_ticked && !*seeded {
          *seeded = true;
          (Some(v.clone()), false)   // write the slot, return did = false
      } else { (None, false) }
  }
  ```

  plus a separate zero-delay inline-emit special case (`interp.rs:1086-1092`).

- **Compiled / nested** — the macro emits a hard-coded `if node.op ==
  OpKind::Delay` branch in `node_dispatch`
  (`wingfoil-next-macros/src/lib.rs:1700-1739`) that reproduces the same
  store-without-tick (`lib.rs:1730-1733`) and the same zero-delay case
  (`lib.rs:1715-1721`), backed by an extra `__seeded_<name>` local declared in
  `node_decl` (`lib.rs:1581-1586`).

This is precisely the **name-based special-case anti-pattern** that the
`Activation` const was introduced to eliminate — see `op.rs:19-20`: the const
"replaces the retrofit's name-based `can_receive_callbacks` allowlist with a
contract." `delay`'s store-without-tick is the last name-keyed behaviour left in
the dispatch paths, and it is duplicated across two engines. `op.rs:76-88`
already documents the gap and explicitly defers the decision to Phase 1:

> If a third op ever needs the same shape, promote it to a real
> `Tick::Silent(T)` variant here and teach all three engines to
> store-without-ticking on it.

### Which future ops need it

Any *store-without-tick* op — an op that must make an initial or interim value
visible to passive readers without firing downstreams:

- `delay` (today) and the whole **delay family** the catalog still owes:
  `delay_with_reset` and `node_flow`'s node-level delay/filter/throttle
  (`docs/port-plan.md:328-329`), which seed and re-seed a held value.
- Any "hold last value / seed initial" op (a `hold`, an initialised `sample`)
  where a passive reader must not observe `Default` before the first real tick.

So it is not delay-specific in principle — it is the general "engine owns the
value slot, but the op wants to write it quietly" shape. Today exactly one op
uses it, which is why option (b) is on the table at all.

### Options

**(a) Add `Tick::Silent(T)` to the contract.** The op expresses
store-without-tick directly; every engine handles it uniformly; the two
name-keyed `delay` special cases are deleted.

**(b) Keep the engine-level special-casing, document it as permanent.** No
contract change; the `OpKind::Delay` branch and the `seeded` flag stay, and every
future store-without-tick op adds another name-keyed branch in both engines.

#### Blast radius of (a)

- **Every `Op::cycle` emitter.** Every interpreted wiring method matches
  `Tick<Out>` and must gain a `Tick::Silent(v) => { *out = v; Ok(false) }` arm.
  Most single-input ops route through the shared `register_op1`
  (`interp.rs:554-561`) — **one** site — but the hand-written ops each have their
  own match: `ticker` (580), `constant` (609), `with_time` (640), `throttle`
  (672), `window` (706), `trimap` (770), `try_trimap` (831), `filter` (862),
  `fold` (894), `sample` (924), `bimap` (988), `try_bimap` (1041), `merge2`
  (1140), `poll` (1175), `finally` (1210), `print` (1251), `timed` (1294),
  `composite` (1418). ≈ 18 arms. Adding the variant makes each match
  non-exhaustive → a compile error at every site, which is the *desired* forcing
  function (same philosophy as the "no `..base`, spell every field" rule at
  `lib.rs:260-262`).
- **All three engines' dispatch.** Interpreted: the arms above. Compiled and
  nested share the macro's `node_dispatch` `on_value` (`lib.rs:1665-1675`), so
  **one** macro site gains the `Silent` arm — `Tick::Silent(__val) => { #value =
  __val; false }`.
- **The compiled/nested emission for `delay` collapses.** The bespoke Delay
  branch (`lib.rs:1700-1739`), the `__seeded_<name>` local
  (`lib.rs:1581-1586`), and the interpreted `seeded` flag
  (`interp.rs:1067-1104`) are all deleted. `delay` becomes an ordinary
  `Inputs::OneTick` op whose `cycle` returns `Silent` on its first value.
- **The zero-delay case moves into `Delay::cycle`.** It is orthogonal to
  `Silent` (it is a `cfg == ZERO` scheduling question, not a store-without-tick
  one), but it is the *other* half of the name-keyed branch. `Delay::cycle`
  already receives the delay (`cfg`), the value + tick flag (`In = OneTick`), and
  `ctx`; for `cfg == ZERO && src_ticked` it can return `Value(v.clone())`
  directly instead of scheduling `time + 0` (which is the fable-review bug 2
  root cause, `fable-review.md:118-119`). Folding it in deletes the last reason
  the macro needs to know the op is named `Delay`.
- **LLVM fold for infallible ops.** The concern is whether a third discriminant
  costs the infallible hot path. It does not, by the same argument the crate
  already relies on for `Ok(..)` (`op.rs:196-202`): after monomorphization an
  infallible op that only ever constructs `Value`/`Quiet` has an unreachable
  `Silent` arm, which is dead-code-eliminated; the match is over a value the
  optimiser can see is never `Silent`. The in-memory discriminant stays a small
  integer either way. No branch survives in the binary for ops that don't use it;
  ops that do use it (delay family) pay one predicted branch, exactly as classic.

### Recommendation: **(a) add `Tick::Silent(T)`**

Option (b) preserves, and invites duplication of, the one name-keyed special case
left in the dispatch paths, in two engines that must not drift — the precise
failure mode `docs/fable-review.md` found repeatedly ("the engines cannot drift"
holds for `Op::cycle` semantics but *not* for engine-owned timing). The delay
family is real upcoming catalog work (`port-plan.md:328-329`), so "only one op
needs it today" will not hold for long, and deciding late means retrofitting every
emitter — the mistake the plan front-loaded fallibility to avoid
(`port-plan.md:256-258`). Promoting it to the contract turns a two-engine,
name-keyed hack into one uniform arm and lets `delay` be written as an ordinary
op.

```rust
// op.rs
pub enum Tick<T> {
    Value(T),
    /// Store this as the node's current value but do NOT tick downstreams.
    /// The engine writes the value slot and reports the cycle as quiet.
    Silent(T),
    Quiet,
}
```

Engine contract for `Silent(v)`: **write the value slot, report `did = false`**
(no downstream propagation, no `ticked[i]` set). Identical in all three engines.

`Delay::cycle` (in `ops.rs`) then owns both behaviours the engines faked:

```rust
fn cycle(cfg, state, (v, src_ticked), ctx) -> Result<Tick<T>> {
    if *cfg == NanoTime::ZERO {
        return Ok(if src_ticked { Tick::Value(v.clone()) } else { Tick::Quiet });
    }
    // pop any due value → Tick::Value; else if this is the first upstream
    // value → Tick::Silent(v.clone()); else Tick::Quiet.
}
```

#### Migration (do `delay` last)

1. Add the `Silent` variant to `op.rs`; update the `op.rs:76-88` doc block from
   "deliberately no `Silent`" to the shipped semantics.
2. Add the `Tick::Silent` arm to `register_op1` and to each hand-written match in
   `interp.rs`, and to the macro's `node_dispatch` `on_value` — all as
   `{ store; did = false }`. (Compile errors guide this; no behaviour changes yet
   because no op emits `Silent`.)
3. Rewrite `Delay::cycle` to emit `Silent` for the first value and to handle
   `cfg == ZERO` inline.
4. Delete the interpreted `seeded` flag / bespoke closure
   (`interp.rs:1067-1104`), the macro Delay branch (`lib.rs:1700-1739`), and the
   `__seeded_<name>` local (`lib.rs:1581-1586`). `delay` now registers like any
   `OneTick` op.
5. Gate on the existing three-engine parity tests — `zero_delay_works` and
   `delay_initializes_to_first_value` (`fable-review.md:234-235`,
   `parity_bugs.rs`) — asserting `interpreted == compiled == nested` before and
   after. Add one row to the table-driven parity suite
   (`port-plan.md:526-537`) exercising a passive read of a delayed slot before
   the delay elapses (the `Silent` observable).

</details>

---

## 2. Per-node `reset`/`setup` hook — re-runnable graphs

**Status: OPEN — the one remaining live proposal.** Verified not yet present on
`next`: `wingfoil-next/src/op.rs` has no `fn reset`/`fn setup`. This recommendation
stands as written.

### The problem, precisely

The `Op` trait has `start`, `stop`, and `teardown`
(`wingfoil-next/src/op.rs:220-239`) but no `reset`. Consequences:

- **`compat::Signal::run` hard-bails on a second run**
  (`wingfoil-next/src/compat.rs:134-145`):

  ```rust
  if self.runner.borrow().is_some() {
      anyhow::bail!("Signal::run called more than once: re-running a graph is
                     not supported (the builder is consumed by the first run)");
  }
  ```

- **The `Runner` itself is single-run for realtime graphs** — the ready channel
  is consumed by the first run (`interp.rs:1569-1574`).
- **Even where a second `Runner::run` is mechanically allowed** (historical/timer
  graphs), it is *incoherent*: accumulator state continues (a counter goes 3 → 6)
  while each run builds a fresh `Kernel` from t=0, so a self-scheduling source
  replays with polluted timing (a ticker fires 0, 400, 500 instead of 0, 100,
  200). This is spike 0.4's finding verbatim (`port-plan.md:210-214`).

But classic streams **re-run**, and the compat facade is exactly the surface that
needs it: `compat::Signal` already breaks (above), and **wingfoil-python's pytest
suite — the facade gate — depends on re-run working** (`port-plan.md:224-232`,
`fable-review.md:276-280`). So this is Phase-1 contract work, not a facade detail.

### The plumbing that already exists to mirror

`reset` slots into the identical machinery as `stop`/`teardown`:

- `NodeRt` holds `start`/`stop`/`teardown` as `LifecycleFn`
  (`interp.rs:149-153`); `push_node` defaults `stop`/`teardown` to no-ops
  (`interp.rs:507-508`) and a node that needs one overwrites the field after
  pushing (e.g. `finally`, `interp.rs:1226-1230`; `timed`,
  `interp.rs:1314-1318`).
- `Runner::run` runs them in fixed phases: `start` (`interp.rs:1586-1591`),
  cycles, `stop` (`interp.rs:1605-1610`), `teardown` (`interp.rs:1611-1616`),
  each with node-context error wrapping.

### The subtlety: "restore to the wiring-time initial value"

A reset must restore each op's state to what it was **at construction**, not to
`Default` and not to its post-run value. Two kinds of initial state exist, and
they must be sourced differently:

1. **Op-intrinsic baseline** — `DelayState::default()`, `EwmaState::default()`, a
   window buffer cleared, a ticker's `last_scheduled = None`, and (for
   self-scheduling sources) *re-arming the first schedule*. The op can restore
   this itself: for a source, `reset` is essentially `start` again.
2. **Config-derived seed** — `fold`'s accumulator is seeded with `init`
   (`interp.rs:885`, `Self::cell(f, init)`) and its value slot with `init.clone()`
   (`interp.rs:884`, `new_slot(init.clone())`). The *op* does not hold `init` at
   reset time (its `Cfg` is the closure `F`); only the wiring site does. So this
   half must be re-seeded by the engine, from a value captured at wiring.

This is why `reset` is best expressed as **both** a trait hook (for the
op-intrinsic part, and to keep the compiled path symmetric) **and** an
engine-owned reset closure (for the config-derived seed + the value slot), exactly
mirroring how `start` is both `Op::start` and a per-node `LifecycleFn` closure.

### Recommendation: concrete signature + plumbing

**Trait hook** (`op.rs`, same shape as `start`, default no-op):

```rust
/// Restore the op's state to its construction-time baseline and re-seed any
/// self-scheduling. Called before `start` on every run, so a `Runner` can be
/// run repeatedly with identical results. A self-scheduling source's `reset`
/// is typically its `start` body (re-arm the first tick); a buffering op
/// clears its buffer. Config-derived seeds (fold's init) are re-seeded by the
/// engine, not here.
#[allow(unused_variables)]
fn reset(cfg: &mut Self::Cfg, state: &mut Self::State, ctx: &mut Ctx<'_>) -> Result<()> {
    Ok(())
}
```

**Engine plumbing** (`interp.rs`):

- `NodeRt` gains `reset: LifecycleFn`, defaulted to a no-op in `push_node`
  (alongside `stop`/`teardown`).
- Each wiring method that has resettable state installs a `reset` closure that
  (a) re-assigns the shared `cs` cell's **state** to a wiring-time-captured
  initial (for `fold`: `init.clone()`, already `B: Clone`, `interp.rs:879`),
  (b) re-assigns the **value slot** to its initial seed (`fold`:
  `init.clone()`; most ops: `Default`), and (c) calls `Op::reset` for the
  op-intrinsic part (a source re-schedules). For a plain source like `ticker`,
  the closure is just `Ticker::reset` (= re-arm), reusing its `start` body.
- `Runner::run` calls each node's `reset` **at the top of every run, before
  `start`** (a new phase loop identical in shape to the `stop` loop at
  `interp.rs:1605-1610`, with `node {i} ({label}) reset` context). On the first
  run this is a harmless "initial := initial"; on subsequent runs it restores
  state. The `Kernel` is already rebuilt fresh each run
  (`interp.rs:1565-1578`), so the clock/schedule queue restart from t=0 — once
  sources re-seed via `reset`, state and clock restart *coherently*, closing the
  0.4 incoherence.
- Per-run scratch is reset too: the shared `ticked` vector and the `finished`
  flag (`interp.rs:235`).

**Scope boundary (documented):** `reset` makes **historical/timer** graphs
re-runnable — the compat / python case. **Realtime** graphs stay single-run: the
waker/ready channel is consumed by the first run (`interp.rs:1569-1574`) and
re-arming it is out of scope for v1 (documented at that bail). This matches the
capability matrix's re-run row (`port-plan.md:62`, notes 8-9).

**`compat::Signal::run` change.** Today it builds the graph and bails on the
second call (`compat.rs:134-145`); the `Signal` already holds a shared
`Rc<RefCell<Option<Runner>>>` (`compat.rs:42-44`). New behaviour: build the
`Runner` **once**, store it, and on every `run` call `runner.run(..)` again —
which is now re-entrant via `reset`. Delete the double-run bail:

```rust
pub fn run(&self, run_mode: RunMode, run_for: RunFor) -> Result<()> {
    let mut slot = self.runner.borrow_mut();
    if slot.is_none() {
        *slot = Some(self.graph.build());   // build once
    }
    slot.as_mut().expect("just built").run(run_mode, run_for)  // reset+run, re-entrant
}
```

(The graph is built once, not per run — avoiding the `GraphBuilder::build(&self)`
+ `mem::take` re-build hazard noted in `fable-review.md:201-204`.)

### Interactions to get right

- **Fold accumulators must reset to `init`, not continue.** This is the whole
  point of the 0.4 finding (`port-plan.md:210-214`): a re-run counter must read 5
  again, not 10. The `fold` reset closure re-seeds both the accumulator cell and
  the value slot to `init.clone()`.
- **Kernel rebuild.** `Kernel::new` per run (`interp.rs:1565-1578`) already
  restarts the clock; `reset` is what makes the *nodes* agree with that restart.
  No Kernel API change is needed.
- **Compiled path is already re-runnable** — `compiled()` is a plain fn, each call
  a fresh independent run (`port-plan.md:87`, matrix note 9). So the trait hook is
  mostly a no-op there; it exists for symmetry and so an op author has one place
  to define reset semantics that the interpreted engine consumes.

### Test plan

- Port classic's re-run tests: run a counter graph to `RunFor::Cycles(5)`,
  `peek_value() == 5`; run **again**, `peek_value() == 5` (not 10). A ticker
  graph run twice fires at the same tick times both runs (0, 100, 200 — the
  polluted-clock regression from 0.4).
- Fold with **non-default** `init` (e.g. 100): re-run returns 100-based result
  again, verified via a passive read (reuses the fold-seed drift fixture from
  `fable-review.md:112-113`).
- `delay`/self-scheduling source re-run: second run's schedule is not polluted by
  the first.
- Three-engine agreement: interpreted-run-twice == `compiled()`-called-twice ==
  same values (compiled is the re-run oracle).
- `compat`: the double-`run`/peek fixtures that currently assert the bail
  (`fable-review.md:248-249`) flip to assert successful re-run.

---

## 3. `supported_ops!()` completeness test — superseded by #496

**Status: superseded by #496 — resolved by construction. No action needed.**
See `docs/macro-extensibility-decision.md`.

This recommendation proposed a single canonical op-list in the macro crate, a
`supported_ops!()` proc-macro exposing it to `wingfoil-next`, and a diff test
against the fluent surface — to stop the macro's hand-maintained supported-op list
from drifting away from the fluent surface (the mechanism by which ~20 ops had
ended up interp-only).

**#496 removed the thing this test would have guarded.** It deleted the `graph!`
macro's `OpKind`/`OpInfo` op-table outright: every op now dispatches through a
single `#[op]`-generated forwarder mechanism plus a generic fallback, so the macro
no longer carries an enumerated op-list at all. It knows only two method names of
its own — `map_n` and `fan` (the topology combinators); every other op — built-in
catalog and user-defined alike — reaches `compiled()` generically, with **no macro
edit**. The hand-written "unknown combinator" error list and the parse-match arms
this section originally cited were part of that deleted table.

With no macro-side op-list, there is nothing for a completeness test to diff
against: the drift this recommendation defended against is now **impossible by
construction** rather than merely tested for. A fluent op cannot be "missing from
the macro" because the macro no longer enumerates ops — an unknown method token is
simply forwarded. So this item is closed, not deferred; nothing replaces it.

The two other recommendations — `Tick::Silent(T)` (§1) and the `reset`/`setup`
hook (§2) — are unaffected by #496 and still stand.

<details>
<summary>Original recommendation (retained for history — superseded by #496)</summary>

### The problem, precisely

There is no single source of truth for "which ops the `graph!` macro supports."
Two hand-maintained lists can drift, and a third gap goes unguarded:

- **The parse-match** — `ChainWalker::apply_call` dispatches combinators by name
  (`wingfoil-next-macros/src/lib.rs:779-935`) and `walk_chain` dispatches sources
  (`lib.rs:712-732`).
- **The "unsupported op" error message** — a *separately hand-written* list
  (`lib.rs:925-934`):

  ```rust
  "unknown combinator `.{other}(..)`; expected one of: map, map_n, fan, filter,
   fold, sample, merge, join, delay, count, accumulate, ewma, ewma_per_tick,
   ewma_half_life, rolling_sum, rolling_mean"
  ```

  Nothing ties this string to the arms above it. Add an arm, forget the string
  (or vice versa) and they silently disagree.

- **Macro-vs-fluent coverage is unguarded.** Nothing catches a fluent op that has
  no `graph!` equivalent — which is exactly how ~20 ops ended up interp-only
  (`port-plan.md:310-319`). The reverse direction (a `graph!` op with no fluent
  method) *is* already guarded by construction: `wire()` re-emits the wiring
  function verbatim (`lib.rs:1443-1445`), so a macro op with no fluent method
  fails to compile `wire()` (`fable-review.md:294`). Only the forward direction
  needs a test.

### Recommendation: one canonical list + a diff test

**Single-source the macro's op list.** Route the combinator dispatch through one
mapping whose domain *is* the supported-op list, and derive the error message from
that same list — so the parse-match and the error can no longer drift:

- Add a canonical list in the macro crate (sources and combinators), e.g.
  `const SUPPORTED_COMBINATORS: &[&str]` and `const SUPPORTED_SOURCES: &[&str]`.
- Build the `apply_call` fallthrough error (`lib.rs:928-932`) and the `walk_chain`
  source error (`lib.rs:723-731`) by joining those lists, replacing the
  hand-written strings.
- Keep `apply_call`'s arms, but assert their coverage against the list with a
  macro-crate `#[test]` that every name in `SUPPORTED_COMBINATORS` is accepted by
  a probe through `apply_call` and every rejected name is absent from it — so an
  arm added without a list entry (or a list entry with no arm) fails the macro
  crate's own tests.

**Expose the list to wingfoil-next.** Add a function-like proc-macro
`supported_ops!()` (in `wingfoil-next-macros/src/lib.rs`, re-exported from
`wingfoil-next`) that expands to a `&'static [&'static str]` literal built from
the same constants:

```rust
// usage in a wingfoil-next test:
const MACRO_OPS: &[&str] = wingfoil_next::supported_ops!();
```

**Diff against the fluent surface.** A new test
`wingfoil-next/tests/supported_ops.rs` compares `supported_ops!()` against the
fluent combinator surface (`StreamOps` + `SourceOps` + `StatisticsOps`,
`fluent.rs` / `stats.rs`) with an **explicit, reviewed allowlist** of methods that
are *not expressible* in `graph!` by design:

```rust
// Fluent methods deliberately absent from graph! (with the reason each is out).
const NOT_EXPRESSIBLE: &[(&str, &str)] = &[
    ("feedback",   "cycle in the DAG breaks straight-line emission (fluent only)"),
    ("external",   "IO-edge source — IO lives at the fluent layer"),
    ("poll",       "busy-poll IO-edge source"),
    ("channel",    "IO-edge source"),
    ("for_each",   "IO-edge sink"),
    ("source",     "raw builder escape hatch"),
    ("inspect",    "side-effecting; not a pure combinator"),
    // fallible/variant/structural ops not yet wired into the macro — each
    // listed with 'planned' vs 'by-design' so the allowlist is a decision log:
    ("try_map", "planned"), ("map_filter", "planned"), ("distinct", "planned"),
    ("difference", "planned"), ("limit", "planned"), ("throttle", "planned"),
    ("window", "planned"), ("buffer", "planned"), ("with_time", "planned"),
    ("ticked_at", "planned"), ("ticked_at_elapsed", "planned"),
    ("not", "planned"), ("print", "planned"), ("timed", "planned"),
    ("join_passive", "planned"), ("join3", "planned"),
    ("try_join", "planned"), ("try_join_passive", "planned"), ("try_join3", "planned"),
    ("rolling_min", "planned"), ("rolling_max", "planned"),
    ("rolling_var", "planned"), ("rolling_std", "planned"),
    // ...seeded from today's gap; each entry is a conscious 'not yet'/'never'.
];
```

The test asserts: **every fluent combinator is either in `supported_ops!()` or in
the allowlist** — and fails otherwise. That is the cheap guard against one-sided
registration: adding a fluent op without either wiring it into the macro or
consciously allow-listing it breaks the build.

### Where it lives

| Piece | Location |
|---|---|
| `SUPPORTED_COMBINATORS` / `SUPPORTED_SOURCES` constants + the single mapping that drives `apply_call` and both error messages | `wingfoil-next-macros/src/lib.rs` |
| `supported_ops!()` function-like proc-macro (expands to `&[&str]`) | `wingfoil-next-macros/src/lib.rs`, re-exported from `wingfoil-next/src/lib.rs` |
| Parse-match-vs-list coverage `#[test]` | `wingfoil-next-macros` (unit test) |
| `supported_ops!()`-vs-fluent diff + `NOT_EXPRESSIBLE` allowlist | `wingfoil-next/tests/supported_ops.rs` |

This is pure guarding — no runtime semantics change — and it is cheapest to land
*before* the Phase 2 catalog volume, so one-sided registration is caught from the
first op added rather than after ~20 have drifted.

</details>

---

## Ratification checklist

Decisions a human must make before implementation begins:

1. ~~**`Tick::Silent(T)`**~~ — **implemented on `next`** (§1,
   `op.rs:85-89`); no ratification needed. The variant landed and `delay`'s
   semantics are expressed through it generically; the name-keyed special case is
   gone.

2. **`reset`/`setup` hook** — ratify the shape and scope? **(the one open item)**
   - [ ] Add `Op::reset(cfg, state, ctx) -> Result<()>` (default no-op).
   - [ ] Add `NodeRt.reset: LifecycleFn`, run before `start` on every run.
   - [ ] Confirm reset semantics = restore state to **wiring-time initial**
         (fold → `init`, not `Default`, not continued) + re-seed value slot +
         re-arm self-scheduling sources.
   - [ ] Confirm scope: re-run enabled for **historical/timer** graphs;
         **realtime** stays single-run (documented).
   - [ ] Rewrite `compat::Signal::run` to build once + re-run (delete the bail).

3. ~~**`supported_ops!()` completeness test**~~ — **superseded by #496**; no
   ratification needed (see §3). #496 deleted the macro's op-table, so there is no
   op-list left to keep in sync with the fluent surface. Nothing to decide.

## Recommended sequencing

Only one Phase-1 contract item remains open. The other two original items have
already resolved on `next`: `Tick::Silent(T)` is **implemented** (§1,
`op.rs:85-89`, via #496), and the `supported_ops!()` completeness test is
**superseded** (§3 — #496 deleted the macro op-table). What is left:

1. **`reset`/`setup` hook before Phase 6.** It mirrors the existing
   `stop`/`teardown` plumbing, but it is the gate for the compat facade and the
   wingfoil-python pytest suite, so it must land before Phase 6 begins. The `Tick`
   contract it would have depended on is already in place (`Tick::Silent` landed),
   so nothing blocks it.

**One-line summary:** of the three original Phase-1 recommendations, two are done —
`Tick::Silent(T)` shipped on `next` (delete of the name-keyed delay special-casing
included) and the `supported_ops!()` completeness test is superseded by #496's
op-table deletion. The **one remaining** recommendation is to add a `reset`
lifecycle hook mirroring `stop`/`teardown` and make `compat::Signal` re-runnable.
