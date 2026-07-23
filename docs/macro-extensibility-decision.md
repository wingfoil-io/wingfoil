# Making the `graph!` op-set extensible — decision & prototype results

**Question.** A framework user cannot add their own op and use it inside
`graph!` / `compiled()` / `nested()` without editing the macro crate. Should
the macro's op-table become optional — an unknown user op flowing through a
generic, trait-driven path so it works in `compiled()` with no macro-crate
edit — and if so, how far do we take it?

**Answer.** Yes — and the branch now goes past option 2 to the end state:
**one mechanism, no table**. The load-bearing uncertainty is resolved: *a
proc macro can emit a monomorphized call to an op whose concrete type it
never names*, at parity with the hand-written table rows it replaced. The
`OpKind`/`OpInfo` table has been **deleted**; every op — the built-in catalog
and user ops alike — dispatches through the same forwarder mechanism, so the
built-ins are now ordinary users of the extension surface (the best ongoing
test of it). The macro knows exactly **two** method names of its own —
`map_n` and `fan`, the topology combinators, which create N nodes
(inexpressible by any per-node mechanism) and take literal counts so the
DAG stays static. Everything else — `count`, `accumulate`, `ewma_per_tick`,
`ewma_half_life` included — is an ordinary op ([`Count`], [`Accumulate`],
[`EwmaPerTick`], [`EwmaHalfLife`] in `ops.rs`), which also removed the last
drift risk: their desugars were previously written twice, once in the
fluent layer and once in the macro; both layers now call the same op.

Everything below is grounded in the prototype commits on this branch:
`wingfoil-next-macros/src/lib.rs` (the fallback), `wingfoil-next/tests/custom_op.rs`
(the proof), `wingfoil-next/benches/custom_op.rs` (the measurement).

---

## 1. The decisive experiment: does the inference trick hold?

**It holds.** The mechanism:

- The macro sees only a method-name *token* (`.delta()`). Instead of erroring,
  it emits calls to **naming-convention forwarder functions** —
  `__wf_op_delta_cycle`, `__wf_op_delta_start` (+ `_owned` variants for
  literal-closure configs) — resolved by ordinary name lookup at the expansion
  site.
- Each forwarder is a *generic* function whose signature is written entirely in
  associated-type projections of the op
  (`&mut <Delta<T> as Op>::State`, `<Delta<T> as Op>::In<'_>`, …) and whose
  body is one line: `<Delta<T> as Op>::cycle(..)`. The `#[op]` attribute
  generates all of them mechanically (implemented; ~60 lines in `expand_op`).
- rustc's inference resolves the op's generics from the argument types at the
  call site — including the node's state local, which the macro declares as a
  bare `let mut __state = Default::default();` with **no type at all**: its
  type exists only as the projection `<Delta<T> as Op>::State`, and unification
  through the forwarder call resolves it (to `Option<f64>` in the test). LLVM
  then monomorphizes the chain exactly like a table row.

`tests/custom_op.rs` proves this for the three shapes that matter, all defined
**outside the library** (user code):

| Shape | Op | What it stresses |
|---|---|---|
| plain config | `Scale` (`Cfg = f64`) | arg→Cfg convention, cfg local |
| generic + state | `Delta<T>` (`State = Option<T>`) | inferred state through projection |
| closure config | `Apply<F>` (`Cfg = F`) | `_cycle_owned` by-value inference deferral (same trick as `cycle_owned_cfg`) |

plus in-crate `.distinct()` — which has `#[op]` but **no table row** — showing
the whole existing `#[op]` catalog reaches `graph!` for free. All of it runs
through **all three expansions** — `interpreted()`, `compiled()`, and a
`nested()` island — with value parity.

### The activation wrinkle (and its fix)

One per-op fact is genuinely needed *before* monomorphization: whether
dispatch needs a `__dirty` check (`Op::ACTIVATION`, a trait const the macro
can't read). First cut used a conservative always-on check: **15% slower**
than the table row. Fix (implemented): `#[op]` re-emits the impl's
`ACTIVATION` expression as a monomorphic const
(`__WF_OP_<NAME>_ACTIVATION`), and the emission guards the check with
`CONST.callback_activated() && __dirty[i]` — constant-folded to nothing for
non-scheduling ops. Same trick composes the `nested()` island's
callback-activation flag at wiring time. This also means **scheduling custom
ops work** (the check is real when `ACTIVATION` says so), not just pure
transforms.

### Benchmark (dense 20-deep chain, 10k cycles, `benches/custom_op.rs`)

| Path | Time | Ratio |
|---|---|---|
| table row (`.map_n(20, ..)`) compiled | 241.1 µs | 1.00× |
| **generic fallback (20 × `.incr()`) compiled** | **243.6 µs** | **1.01×** |
| same custom chain, interpreted | 2.42 ms | ~10× |

Within noise. The generic path costs nothing after monomorphization — as
predicted, LLVM cannot tell it from a hand-written row — and it unlocks the
full compiled/interpreted gap (~10×) for user ops.

## 2. "Could the macro run the interpreted graph and interrogate it instead?"

No — not in a single compilation. A proc macro expands *before* name
resolution and type checking; rustc offers no API to ask "what type is this
expression". The two approximations both exist in this repo's history:

1. **Two-phase build-script codegen** (classic `wingfoil::codegen` /
   `wingfoil-codegen-build-example`): `build.rs` runs the wiring against the
   interpreted builder, which records metadata, then generates runner source.
   It genuinely interrogates the built graph — but only at the fidelity a
   running program can report: **types come back as strings, closures cannot
   be recovered at all**. Those are walls #1–2 in `wingfoil-next/src/lib.rs`
   that this crate exists to escape; re-adopting them for extensibility would
   reintroduce exactly the drift the Op-pattern eliminated.
2. **Let the compiler be the interrogator** — the fallback above. The macro
   emits type-agnostic code; inference + monomorphization answer every
   type-level question, and re-emitted consts answer the const-level one
   (activation) after folding. This is strictly stronger: closures survive,
   and there is no second build phase.

## 3. `OpInfo`, field by field: trait-derivable vs token-bound

(Note: the current table has no `has_stop`/`has_teardown` fields — `compiled()`
today emits no stop/teardown for *any* op. The fallback inherits that
pre-existing gap; forwarder-based `_stop`/`_teardown` emission is the same
mechanical follow-up for both paths.)

| Field | Verdict | How |
|---|---|---|
| `op_type` | **eliminated** | forwarder naming convention; inference names the type |
| `callback_activated` | **derived** (implemented) | `ACTIVATION` re-emitted as const by `#[op]`; guard folds post-mono |
| `has_start` / `state_in_start` | **derived** (implemented) | call `_start` unconditionally through forwarders; the default no-op inlines away |
| `owned_closure` | **derived** (implemented) | call-site syntactic fact (literal closure vs other expr) — decided generically from tokens |
| `cfg_init` | **convention** (implemented) | the single call argument *is* the `Cfg` value; unification handles literals. (`NanoTimeFrom` convenience rows stay table/sugar) |
| `state_init` | **convention** (implemented) | `Default::default()`, type inferred — same contract as `register_op1`. `fold`'s call-arg seed stays a table row |
| `value_seed` | **convention** | `Default` — matching interpreted. `CloneState` (fold's correctness-critical pre-first-tick `init`) stays a table row |
| `unit_output` | residue | needs `Out = ()` knowledge; only ticker — sources are outside fallback scope anyway |
| `inputs` shape | **convention** (implemented) | `(receiver, edges…)` values-only, all active — see below; tick-flag/passive inputs not derivable from tokens |
| `edges` | **convention** (implemented) | `AllActive`; `OneActive` (sample's passive edge) stays a table row |

**Net residue** = sources (ticker/constant), tick-flag inputs (delay, merge),
passive edges (sample), non-default seeding (fold, delay), and parse-level
sugar (`count`, `accumulate`, `map_n`, `fan`, `ewma_*` spellings). The table
**stops growing**: every future single- *or* multi-input values-only op needs
zero macro edits. Four current rows (`Ewma`, `RollingSum`, `RollingMean`,
`Join`) are already deletable via the fallback; left in place on this branch
to keep the prototype diff reviewable.

### The multi-input convention (implemented and proven)

At a call `.my_join(&other, f)`, the fallback classifies each argument at
expansion time: an argument of the exact form `&name` (or a bare
input-parameter `name`) where `name` is a **stream bound in this graph** is an
*edge*; everything else is config. This is decidable and unambiguous because
the macro already tracks bound stream names, already rejects shadowing, and
already forbids stream identifiers inside non-wiring code. Edges are active
and values-only: `In = (&receiver, &edge…)` in call order — the `join` shape —
with at most one config argument (a literal closure goes through
`_cycle_owned` as before). Cost: `Inputs::Many(n)` in the emitter plus the
classification loop (~40 lines), and a public `register_op2` so out-of-crate
ops can wire the interpreted path.

Proven in `tests/custom_op.rs` by two user ops through all three engines:
`Spread` (two inputs, no config) and `Combine<A, B, C, F>` — a fully generic
user-defined join (`.combine(&other, |a, b| a + 10.0 * b)`, two inputs **and**
a closure config, every type inferred) — including a two-input `nested()`
island. Multi-input is the same monomorphized emission as single-input (one
more tuple element), so the 1.01× benchmark result carries over unchanged.

Passive edges and tick-flag inputs stay hand-written table rows (rare and
correctness-critical); an op needing them runs interpreted or gets a row.

One inference caveat, not specific to the fallback (built-in `join` defers
closure inference the same way): with heavy generic-math crates in the linked
impl universe (nalgebra via the classic crate's augurs adapter under
`--all-features`), an unannotated two-arg closure body like `a + 10.0 * b`
can overflow trait search (E0275) before the input types unify. Annotating
the closure params (`|a: &f64, b: &f64| …`) resolves it and is what user code
would naturally write.

## 4. Blast radius of option 2 (measured — it *is* this branch's diff)

| File | Change |
|---|---|
| `wingfoil-next-macros/src/lib.rs` | +~310/−45: fallback arm, `NodeDef::info`, forwarder + const emission in `#[op]` |
| `wingfoil-next/src/interp.rs` | `register_op1` `pub(crate)` → `pub` (+doc), new `pub register_op2` (the join shape) — without these even the *interpreted* path was closed to out-of-crate ops (`#[op]` emits an inherent `impl Builder`, in-crate only) |
| `tests/custom_op.rs`, `benches/custom_op.rs` | new (proof + measurement) |
| `tests/trybuild/unknown_combinator.*` | error changed: first error is now the friendly `E0599: no method named `frobnicate` found for Stream<u64>` at the call site |

Zero table rows edited; zero engine-semantics changes; full suite + clippy
(`lint`, `lint-all`) green.

### Remaining work to productize (not blocking the decision)

1. **`#[op]` out-of-crate**: the attribute still emits `impl crate::interp::Builder`
   (in-crate only). Fix: `extern crate self as wingfoil_next;`, emit
   `::wingfoil_next::` paths, and generate the interpreted wiring as an
   extension trait instead of an inherent impl. Until then a user hand-writes
   the four forwarders + const + fluent method (~40 mechanical lines — see the
   test); after it, a user op is `impl Op` + `#[op(build = name)]` + a 3-line
   fluent method. 
2. **stop/teardown in compiled()** — pre-existing gap, same forwarder pattern.
3. **Collision hygiene**: a denylist for `Stream`'s own inherent methods
   (`clone`, `handle`, `wire`) so typos there keep a curated error.
4. **`#[op]` for multi-input ops**: the derive currently generates
   forwarders for the single-input `In` shape only; extending it to emit the
   n-ary forwarders + a `register_op2`-based builder method is the same
   mechanical pattern (the macro-side emission already handles any arity).

## 5. Convergence: how the residue was absorbed (the table is deleted)

Each table-only capability moved to the trait/derive layer, exactly as
§3 predicted:

| Former table capability | Where it lives now |
|---|---|
| input shapes (value-only / tick-flag / pairs) | the macro always passes one `(value, tick)` pair per edge; the `#[op]` derive parses `In`'s tokens and emits the adapting forwarder — shape knowledge lives with the type |
| activation / dirty checks | `__WF_OP_<M>_ACTIVATION` const, re-emitted by `#[op]`, folded post-mono (also composes the island's callback flag and supports `always` busy-poll ops) |
| passive edges (sample) | `__WF_OP_<M>_PASSIVE` bitmask const, folded into dispatch conditions and the island's input-activation list |
| state/value seeding (fold) | `_seed_state` / `_seed_value` forwarders — structural generics so nothing dangles; fold hand-writes them to clone `init` into both (classic parity preserved) |
| delay's zero-delay inline emit + silent first-value store | promoted into the `Op` contract as `Tick::Silent(T)` (the promotion `op.rs` had reserved); `Delay::cycle` now expresses its full semantics once, and all three engines handle `Silent` generically |
| sources (ticker/constant) | same mechanism, rooted at builder methods; `Ticker`/`Delay` `Cfg` became the call-site `Duration` (arg-verbatim convention), with ticker caching the converted period in state at `start` |
| `unit_output`, `CfgInit`, `StateInit`, `ValueSeed`, `Edges`, `Inputs`, `OpKind`, `OpInfo` | deleted |
| `count` / `accumulate` / `ewma_per_tick` / `ewma_half_life` parse arms | retired into real ops (`Count`, `Accumulate`, `EwmaPerTick`, `EwmaHalfLife`); the fluent layer calls the same ops, single-sourcing desugars that previously existed in two places |

Two conventions carry the start hooks: `_start` forwards the real
`Op::start` only when the impl overrides it (otherwise it is a fully-erased
no-op — a forwarding version would dangle op generics that `Cfg`/`State`
don't mention, e.g. filter's `T`); closure-config ops get `_start_owned`,
which cannot see the closure (a duplicate literal would have un-inferable
parameter types), so a closure-config op with a real `start` hook
hand-writes its forwarders.

Known limitations of the unified conventions (accepted, documented):
- an op mixing a *non-literal* closure (factory/named local) with other
  plain config args (fold with a closure factory) is not expressible — the
  literal-closure and named-closure forms both work;
- at most one literal closure argument per call;
- `_start_owned` cannot pass the closure to a start hook.

Benchmark after deletion (same dense 20-chain): the two bench variants are
now literally the same mechanism and measure identical; against the
pre-deletion table emission, interleaved A/B runs show parity within ~3%
with overlapping confidence intervals on a host drifting ±5% between runs
(the earlier stable-host measurement of the same mechanism was 1.01×).

## 6. Recommendation

**Option 2.** Option 1 (islands only) leaves the single most common extension —
a user transform in a hot compiled graph — behind a per-activation dyn
boundary for no reason now that the fallback is measured at 1.01×. Option 3
(full type-level graph) buys nothing further on performance (already 1×),
destroys the "wiring fn is plain, valid Rust" property that makes `graph!`
reviewable, and pays the well-known type-level costs (DAG fan-in/sharing/
feedback as HList/index gymnastics, brutal error messages) to delete a table
that option 2 has already reduced to a static, non-growing residue of
genuinely exotic wiring.

Migration path: (a) fallback + forwarders + const guards + multi-input
convention [done]; (b) absorb the residue and delete the table [done — §5];
(c) `#[op]` out-of-crate; (d) stop/teardown emission.
The escape hatch (`nested()` islands for interpreted-only ops) remains for
everything the residue still excludes.
