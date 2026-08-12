# wingfoil-derive

The proc macros behind [`wingfoil`](../wingfoil/): **`nitro!`** and
**`#[op]`**.

Between them they deliver the engine's central promise — one wiring definition,
three execution tiers, no duplicated execution logic — and they do it without any
per-op table that a new op would have to be registered in.

## `nitro!` — a fluent wiring function in, every engine out

The macro body is a single, *valid Rust* function written against the fluent API:
it takes the builder as a parameter, wires streams with ordinary `let` chains,
and returns its output stream(s). The macro parses that function, derives the DAG
from the method chains, and expands to a module named after it.

```rust,ignore
wingfoil::nitro! {
    pub fn evens_sum(g: &GraphBuilder) -> Stream<u64> {
        let count = g.ticker(PERIOD).count();
        let is_even = count.map(|i| i.is_multiple_of(2));
        count.filter(&is_even).fold(0u64, |acc, v| *acc += v)
    }
}

let (mut runner, sum) = evens_sum::interpreted();
let (sum2,)           = evens_sum::compiled(run_mode, run_for);
```

The generated module contains four entry points:

| Entry point | What it is |
|---|---|
| `wire(g)` | Your function, **verbatim** (renamed), reusable as ordinary fluent wiring — and the composition seam for nesting one `nitro!` inside another. |
| `interpreted()` | The graph built through `wire`, returning a `Runner` plus a typed handle per output. |
| `compiled(run_mode, run_for)` | A fully monomorphized runner derived from the same tokens: node state in locals, tick propagation as `bool`s, every `Op::cycle` call — closures included — visible to the compiler. |
| `nested(g, inputs...)` | The whole graph mounted as a **single compiled node** (an "island") inside an interpreted graph. One closure owns all inner state; the outer engine pays one dyn call per activation for the entire sub-graph. |

### `compiled()` vs `nested()`

Both are generated from the same tokens and call the same `Op::cycle` code, so
they are semantically identical. They differ in **who owns the run loop**:

- **`compiled()` is the whole program.** A standalone function owning its own
  `Kernel`, running the cycle loop to completion and returning the declared
  outputs. Because LLVM sees the whole graph as one function it fuses across node
  boundaries and constant-folds — that is where the speed-up comes from. The price
  is that it is a closed box: static topology, outputs only, no I/O.
- **`nested()` is a component.** The hot core is compiled while the edges stay
  open, so threaded sources, feedback and adapters still work around it. Inner
  schedules (tickers, delays) are demultiplexed through a private queue, with only
  the earliest forwarded to the outer kernel.

### The constraint

`nitro!` reads tokens to derive a **static DAG** at expansion time; it does not
run the code. So wiring must be straight-line — the *shape* of the graph cannot
depend on runtime values, though values and per-element logic can be as
procedural as you like.

[`examples/core/dual_mode/`](../wingfoil/examples/core/dual_mode/) is the
reference for exactly what is and isn't accepted, and prints an abridged copy of
the generated code.

## `#[op]` — no macro table to edit

`#[op(build = name)]` on an `Op` impl generates two things: the interpreted
`Builder` method, and the naming-convention forwarders that compiled and nested
emission dispatch through.

That is what makes the engine extensible without touching it. `nitro!` never
names an op type — rustc's inference resolves each from the argument types at the
call site, and the per-op activation consts fold into the tick gates after
monomorphization. A **user-defined** op therefore takes the identical path to a
built-in one, and gets interpreted *and* compiled coverage for free.

## Working on these macros

Proc-macro crates must be their own compilation unit, which is why this is a
separate crate rather than a module of `wingfoil`.

To see what an expansion actually produces:

```sh
cargo expand --manifest-path crates/wingfoil/Cargo.toml --example dual_mode
```

Adding an op is covered by the `/new-op` skill and
[`docs/planning/port-plan.md`](../../docs/planning/port-plan.md) § "Adding an op".

## See also

- [`../README.md`](../README.md) — the crate map for `crates/`
- [`../wingfoil/`](../wingfoil/) — the engine these macros serve
- [`../wingfoil-python-derive/`](../wingfoil-python-derive/) — the same idea for the Python bindings
- [`../../docs/decisions/macro-extensibility-decision.md`](../../docs/decisions/macro-extensibility-decision.md) — why there is no per-op table
