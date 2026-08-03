# Lines Adapter Example (wingfoil)

The line-oriented file adapter end to end: replay a text file through a graph,
transform it, and write the result to another file — the smallest complete
Op-pattern I/O edge in both directions.

No serde, no schema, no server: if you want to read what a wingfoil adapter
*is* with nothing else in the way, read this one.

## Run

The lazy historical replay source is behind the `async` feature (like
`csv_read`), so the example requires it. No other prerequisites — it stages its
own input file in the OS temp directory.

```sh
cargo run --manifest-path crates/wingfoil/Cargo.toml --example lines_adapter --features async
```

## Code

```rust
let g = GraphBuilder::new();

// Deterministic historical replay: one record per successive graph instant.
let lines = replay_lines(&g, &input, None)?;

let shouted = lines.map(|burst: &Burst<String>| {
    burst.iter().map(|s| s.to_uppercase()).collect::<Burst<String>>()
});

let _sink = shouted.write_lines(&output)?;

let mut runner = g.build();
runner.run(RunMode::HistoricalFrom(NanoTime::ZERO), RunFor::Forever)?;
```

`replay_lines` has no timestamp column to read, so it assigns each record a
successive graph instant — record *n* lands at `NanoTime::new(n)`. That is enough
to make the replay deterministic and ordered, which is all a line-oriented file
can promise.

## Output

Input written by the example:

```text
alpha
bravo
charlie
delta
```

After the run:

```text
replayed records at their graph timestamps:
  0: ["alpha"]
  1: ["bravo"]
  2: ["charlie"]
  3: ["delta"]

wrote /tmp/wingfoil_lines_out_21959.txt:
  ALPHA
  BRAVO
  CHARLIE
  DELTA
```

## See also

- [`csv`](../csv/) — the same shape with typed rows and real event timestamps.
- [`src/adapters/lines/`](../../../src/adapters/lines/) — the adapter itself, and
  its `CLAUDE.md` on how a source/sink pair is put together.
