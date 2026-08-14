## Odds and evens — split and recombine

The textbook non-linear DAG: a counter is split by parity into two labelled
branches, then merged back into one stream. Written in the **builder-less**
style — `ticker` is a free source function and the stream runs directly, with no
`GraphBuilder` or `Runner` to hold. Output flows through `logged`, so each value
carries the engine time.

```rust
let count = ticker(Duration::from_millis(10)).count();

let evens = count
    .filter(&count.map(|i| i % 2 == 0))
    .map(|i| format!("{i} is even"));
let odds = count
    .filter(&count.map(|i| i % 2 == 1))
    .map(|i| format!("{i} is odd"));

odds.merge(&evens)
    .logged("odds/evens", log::Level::Info)
    .run(RunMode::HistoricalFrom(NanoTime::ZERO), RunFor::Cycles(6))?;
```

### The two structural facts

- **`count` is a shared apex node.** Both branches read it; the engine runs it
  once per cycle and fans the tick out to every reader — it does not re-run per
  downstream path.
- **`merge` is the recombine.** A number is either odd or even, so at most one
  branch fires on any given tick — `merge` passes through whichever did.

### Output

`logged` emits through the `log` crate; rendered by `env_logger` (the volatile
wall-clock prefix shown as `[..]`):

```text
[.. INFO  wingfoil] 0.000_000 odds/evens "1 is odd"
[.. INFO  wingfoil] 0.010_000 odds/evens "2 is even"
[.. INFO  wingfoil] 0.020_000 odds/evens "3 is odd"
[.. INFO  wingfoil] 0.030_000 odds/evens "4 is even"
[.. INFO  wingfoil] 0.040_000 odds/evens "5 is odd"
[.. INFO  wingfoil] 0.050_000 odds/evens "6 is even"
```

### Run

```sh
RUST_LOG=info cargo run --manifest-path crates/wingfoil/Cargo.toml --example odds_evens
```

### Where to go next

- [`dual_mode`](../dual_mode/) — the same split/recombine shape through `nitro!`,
  run interpreted vs compiled and asserted equal.
- [`topological_sort`](../topological_sort/) — why the shared apex node matters.
- [`hello_graph`](../hello_graph/) — the linear starter graph.
