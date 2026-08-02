## Fan-out 10×10 — the benchmark shape through `nitro!`

The `benches/graph.rs` "10x10" graph expressed through the `nitro!` macro: one
`count` source fanned out into 10 parallel 10-deep identity-`map` chains, then
merged back into one stream. 100 `map` nodes plus the source and the merge.

This is the shape the engine benchmarks measure, so it is the useful one to look
at when asking what the compiled tier actually buys you: the example runs both
engines and checks they agree.

### Why the nodes are spelled out literally

`nitro!` requires straight-line wiring — the DAG must be static — so the 100
`.map()` nodes **cannot** be built with a `for` loop the way the classic fluent
wiring does:

```rust,ignore
// Fine in the fluent API, impossible inside nitro!:
for _ in 0..10 { chain = chain.map(|i| i); }
```

They are instead written out literally in the shared
[`bench_support/fanout_10x10.rs`](../../../bench_support/fanout_10x10.rs), which
this example `include!`s so the benchmark and the example derive from exactly the
same tokens. From those tokens the macro derives both engines.

If you want static repetition without writing it out, `.map_n(N, ..)` and
`.fan(N, ..)` take a **literal** count and unroll at expansion time — see
[`dual_mode`](../dual_mode/) for the full list of what `nitro!` does and does not
accept.

### Output

```text
10x10 fanout (Cycles(1000)): interpreted = compiled = 1000
```

### Run

```sh
cargo run -p wingfoil-next --release --example fanout_10x10
```

Use `--release` if you care about the timing — the interpreted/compiled gap is
mostly an optimizer story and a debug build flatters neither tier.

### Where to go next

- [`dual_mode`](../dual_mode/) — the rules governing what `nitro!` accepts.
- [`breadth_first`](../breadth_first/) — why a 100-node fan-out costs 100 node
  visits per tick and not 2^N.
- [`benches/`](../../../benches/) — the measured comparison of the three tiers.
