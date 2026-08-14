## Dual mode — one wiring, two engines

The dual-mode thesis, completed by the `nitro!` macro: **one** wiring definition
expands to both an interpreted runner and a fully monomorphized compiled runner.
They cannot drift — same tokens, same `Op` semantics — and the compiled one gets
the compiler's full optimization across node boundaries.

The wiring is the canonical split/recombine DAG — a counter split on parity into
two labelled branches, merged back into one stream:

```text
              count                 (apex — shared node, once/cycle)
             /     \
      is_odd?      is_even?         (split on parity)
           |          |
     "{i} is odd" "{i} is even"     (format each branch)
             \     /
              merge                 (recombine — at most one fires/tick)
                |
              print
```

The run asserts *that* the two engines agree. The rest of this example is the
reference for *what you are allowed to write* to get that guarantee — and it
ends with an abridged rendering of the generated code, so "straight-line wiring
becomes a static schedule" is something you can read rather than take on faith.

### Choosing an engine: `run(tier, ..)`

`interpreted()` and `compiled()` are shaped differently on purpose — the first
hands back a `Runner` plus handles you read *after* running, the second takes
the run bounds and returns values. That difference is what would otherwise stop
you swapping engines behind a flag, so the macro emits a third entry point that
reconciles them:

```rust,ignore
pub fn run(tier: Tier, run_mode: RunMode, run_for: RunFor) -> Result<(Out, ..)>
```

The engines are held to identical values *and* tick times, so the tier only ever
changes *how* the graph runs. Develop against `Tier::Interpreted` — it carries
per-node error context and honours the `instrument-*` span features, and it is
the only tier that supports dynamic graph surgery — and deploy on
`Tier::Compiled`.

`Tier::default()` picks between them from the `WINGFOIL_TIER` environment
variable, falling back to the build profile (interpreted in debug, compiled in
release). Both engines are in the binary either way, so that variable flips
tiers **without a rebuild** — a release binary misbehaving in the field can be
re-run once under `WINGFOIL_TIER=interpreted` to get node-labelled errors and
engine spans out of the same executable.

### When a node fails

Every tier names the failing node and the lifecycle hook it failed in. The
monomorphized tiers can do slightly better than the interpreted one, because the
macro knows the **binding you wrote** where the interpreted engine only has the
op's `type_name`:

```text
interpreted   node 5 (Map) cycle: <the op's error>
compiled      node 5 (odd_str: map) cycle: <the op's error>
nested        island node 5 (odd_str: map) cycle: <the op's error>
```

Intermediate (unnamed) nodes fall back to their `wf_anon_N` slot name, which is
the same name the generated code and the node table below use. The label is a
`&'static str` baked in at expansion time, so it costs nothing until something
actually fails.

### The one rule

`nitro!` parses its body as a plain Rust `fn`, but it does not *run* that code —
it reads the tokens to derive a **static DAG** at expansion time, then re-emits
the schedule three ways (`interpreted` / `compiled` / `nested`). Because
`compiled()` monomorphizes one local per node, the node list must be complete and
fixed after parsing:

> **Wiring must be straight-line — the shape of the graph cannot depend on
> runtime values.** Values and per-element logic can be as procedural as you
> like; the *topology* cannot.

Each top-level statement is sorted into one of three buckets: a **wiring**
`let name = <chain>;` (rooted at the builder or an already-bound stream), the
**tail** expression naming the outputs, or **passthrough** (anything else,
re-emitted verbatim into every engine). The builder and stream names may appear
only in wiring statements and the tail.

#### ✅ Allowed

```rust,ignore
// Straight-line wiring: each `let` is one fluent chain.
let count  = g.ticker(PERIOD).count();
let parity = count.map(|i| i % 2);

// Passthrough: ordinary Rust that does NOT mention `g` or a stream.
let base = 2;
let threshold = base * 4;
let tagged = count.filter(&parity).map(move |i| i + threshold);

// Any control flow you want *inside* an op closure — opaque config, never topology.
let label = count.map(|i| if i % 2 == 0 { "even" } else { "odd" });

// Static repetition sugar with a LITERAL count.
let chained = count.map_n(3, |i| i + 1);
let fanned  = count.fan(2, |s| s.map(|i| i * 10));
```

#### ❌ Not allowed

```rust,ignore
// A helper that does wiring — the macro cannot see the nodes it builds.
// Compose by NESTING nitro!s instead: each nitro! fn is reusable wiring via its `wire`.
let x = build_subgraph(g, &count);

// A loop that wires — node count would be a runtime value.
// Use `.map_n`/`.fan` with a literal instead.
for _ in 0..n { count = count.map(|i| i + 1); }

// A conditional that picks the TOPOLOGY.
// Branch *inside* a closure instead, or build both and select at runtime.
let s = if fast { count.ema(2) } else { count.ema(8) };

// A non-literal repeat count — the unrolled DAG must be known statically.
let chained = count.map_n(n, |i| i + 1);
```

### When a method is rejected

Dispatch is by naming convention, not a table, so a method `nitro!` cannot
dispatch shows up as an unresolved *forwarder*. Three cases:

**A name reserved by `Stream`'s inherent interface** — `clone`, `handle`,
`graph`, `wire`, `value_slot`, `build` or `upstream`. These are not combinators
at all, so they cannot identify an op: fluent wiring would resolve the inherent
method while compiled emission resolved a same-named forwarder, and the two
tiers would mean different things. One message, at your call site:

```text
error: `.build()` is reserved by `Stream`'s inherent interface and cannot be
       used as a `nitro!` op name: it consumes the whole graph into a `Runner`
       rather than adding a graph node — a `nitro!` block wires the graph, and
       the caller builds and runs it
```

`.build()` is the one worth knowing about: it is how you close a *fluent* chain
(`.print().build().run(..)`), so it is easy to carry into a `nitro!` block by
habit. A `nitro!` block only wires — the caller builds and runs what it returns.

**A method that cannot be an op** — `split` (two outputs, where an `Op` has one
`Out`), `feedback` (a cycle), or `collapse_accumulate` / `filter_none` (sugar
over `fold` / `map_filter`). One message, naming the replacement:

```text
error: `.split(..)` has no `nitro!` forwarder, so it cannot appear in a compiled
       graph: it is sugar over two `map`s — bind them separately:
       `let a = pairs.map(|t| t.0.clone()); let b = pairs.map(|t| t.1.clone());`
```

The list is short on purpose: sugar that *can* become an op is promoted instead.
`not` and `collapse` were both rejected here until they became real ops and
started working in `nitro!` outright.

**A typo, or an op with no `#[op(build = …)]`** — rustc's `no method named` plus
two or three `cannot find value __WF_OP_<NAME>_…` errors:

```text
error[E0425]: cannot find value `__WF_OP_FROBNICATE_ACTIVATION` in this scope
error[E0599]: no method named `frobnicate` found for struct `Stream<T>`
```

Read the **`E0599`** — the `__WF_OP_*` errors are its echo, and the "a constant
with a similar name exists" suggestion on them is noise (it offers to replace
your call with an internal constant; never the fix). The macro cannot narrow
this further without a per-op table, which is exactly what the open-op-set
design removes — see [`docs/decisions/macro-extensibility-decision.md`](../../../../../docs/decisions/macro-extensibility-decision.md).

### Reading the generated code

The bottom of [`main.rs`](main.rs) carries an abridged rendering of the full
expansion — `wire`, `interpreted`, `compiled`, and `nested`. The DAG is 10 nodes,
indexed in wiring order; intermediate (unnamed) nodes get `wf_anon_N` slots while
your `let` names are kept verbatim. Every op — built-in or user-defined — is
dispatched through naming-convention forwarders, so the macro never names an op
type: rustc's inference resolves each from the argument types at the call site,
and the per-op activation consts fold into the tick gates after monomorphization.
That is why `#[op]` gives a user-defined op compiled coverage with no macro table
to edit.

For the unabridged version:

```sh
cargo expand --manifest-path crates/wingfoil/Cargo.toml --example dual_mode
```

### Output

```text
1 is odd
2 is even
3 is odd
...

10 labels over 10 cycles — interpreted and compiled engines agree.
`run(Tier::default(), ..)` resolved to the compiled tier and matched them.
```

(That last line reads `interpreted` in a debug build, or under
`WINGFOIL_TIER=interpreted` — same values either way, which is the point.)

### Run

```sh
cargo run --manifest-path crates/wingfoil/Cargo.toml --release --example dual_mode
```

### Where to go next

- [`../../benches/tiers.rs`](../../../benches/tiers.rs) — the measured cost of
  each tier, over this graph and the 100-node fan-out shape in
  [`../../bench_support/fanout_10x10.rs`](../../../bench_support/fanout_10x10.rs)
  (which is also the worked example of `map_n` / `fan`, the static repetition
  sugar the ❌ list above points at).
- [`topological_sort`](../topological_sort/) — why the schedule is ordered the
  way it is in the first place.
