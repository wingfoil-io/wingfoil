## Odds and evens — split and recombine

The textbook non-linear DAG: a counter is split by parity into two labelled
branches, then merged back into one stream. It is the graph the parity tests
use, shown here as a runnable program.

Written **once** as a `nitro!` wiring function, it expands to both engines —
`interpreted()` and `compiled()` — and the example asserts they produce
identical output. That assertion is the dual-mode guarantee in miniature.

```rust
wingfoil_next::nitro! {
    fn odds_evens(g: &GraphBuilder) -> Stream<Vec<String>> {
        let count    = g.ticker(PERIOD).count();
        let is_even  = count.map(|i| i.is_multiple_of(2));
        let is_odd   = is_even.map(|b| !b);
        let odd_str  = count.filter(&is_odd).map(|i| format!("{i} is odd"));
        let even_str = count.filter(&is_even).map(|i| format!("{i} is even"));
        let acc      = odd_str.merge(&even_str).accumulate();
        acc
    }
}

let (mut runner, acc) = odds_evens::interpreted();
runner.run(HISTORICAL, run_for)?;
let interpreted = runner.value(acc);

let (compiled,) = odds_evens::compiled(HISTORICAL, run_for)?;
assert_eq!(interpreted, compiled, "both engines must agree");
```

### The two structural facts

- **`count` is a shared apex node.** Three statements read it. The interpreted
  engine executes it *once* per cycle and fans the tick out to every reader; the
  compiled engine emits it once and feeds all readers from the same slot. Neither
  re-runs it per downstream path.
- **`merge` is the recombine.** A number is either odd or even, so at most one
  branch fires on any given tick — `merge` passes through whichever did.

### Output

```text
1 is odd
2 is even
3 is odd
4 is even
5 is odd
...

10 labels — interpreted and compiled engines agree.
```

### Run

```sh
cargo run -p wingfoil-next --example odds_evens
```

### Where to go next

- [`dual_mode`](../dual_mode/) — the same wiring, plus the rules for what you may
  write inside `nitro!` and an abridged dump of the generated code.
- [`fanout_10x10`](../fanout_10x10/) — the same idea at 100 nodes.
- [`breadth_first`](../breadth_first/) — why the shared apex node matters.
