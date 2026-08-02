## Dual mode — one wiring, two engines

The dual-mode thesis, completed by the `nitro!` macro: **one** wiring definition
expands to both an interpreted runner and a fully monomorphized compiled runner.
They cannot drift — same tokens, same `Op` semantics — and the compiled one gets
the compiler's full optimization across node boundaries.

The wiring is the same split/recombine DAG as [`odds_evens`](../odds_evens/):

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

`odds_evens` shows *that* the two engines agree. This example is the reference
for *what you are allowed to write* to get that guarantee — and it ends with an
abridged rendering of the generated code, so "straight-line wiring becomes a
static schedule" is something you can read rather than take on faith.

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
cargo expand -p wingfoil-next --example dual_mode
```

### Output

```text
1 is odd
2 is even
3 is odd
...

10 labels over 10 cycles — interpreted and compiled engines agree.
```

### Run

```sh
cargo run -p wingfoil-next --release --example dual_mode
```

### Where to go next

- [`fanout_10x10`](../fanout_10x10/) — the same macro on the 100-node benchmark shape.
- [`odds_evens`](../odds_evens/) — the minimal version of this graph.
