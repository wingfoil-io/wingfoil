# CSV Adapter Example (wingfoil)

The CSV adapter end to end: replay a CSV file as a deterministic historical burst
stream, transform each row, and write the result back to a CSV file.

CSV is the adapter worth reading first — it needs no server, and the replay
machinery it uses (lazy `produce_async` + `buffer_size`) is the same one behind
the [`lines`](../lines/) adapter and the timestamped async sources.

## Run

No prerequisites — the example stages its own input file in the OS temp
directory.

```sh
cargo run --manifest-path crates/wingfoil/Cargo.toml --example csv_adapter --features csv
```

## Code

Rows are typed. serde field names become the CSV header columns, and a
caller-supplied closure extracts the event time from each row — that timestamp is
what drives the graph clock during replay, so the replay honours the *data's* own
schedule rather than the wall clock.

```rust
/// A named-field record — serde field names become the CSV header columns.
#[derive(Debug, Clone, Default, serde::Serialize, serde::Deserialize)]
struct Quote {
    timestamp: u64,
    price: f64,
}

let g = GraphBuilder::new();

// `false` = the input has no header row; `None` = default buffer size.
let rows = csv_read(&g, &input, |q: &Quote| NanoTime::new(q.timestamp), false, None)?;

let bumped = rows.map(|b| {
    b.iter()
        .map(|q| Quote { timestamp: q.timestamp, price: q.price + 1.0 })
        .collect::<Burst<Quote>>()
});

let _sink = bumped.csv_write(&output)?;

let mut runner = g.build();
runner.run(RunMode::HistoricalFrom(NanoTime::ZERO), RunFor::Forever)?;
```

Two things to note:

- **`rows` is a `Stream<Burst<Quote>>`.** Rows sharing a timestamp arrive
  together in one burst, never coalesced — so a file with three ticks at the same
  microsecond replays as three values, not one.
- **`csv_write` is a sink.** It returns a handle you keep alive (`_sink`); the
  write is driven by the graph, on the graph's clock.

## Output

Input staged by the example:

```text
100,10.0
200,11.5
300,9.75
```

After the run:

```text
wrote /tmp/wingfoil_csv_adapter_out.csv:
time,timestamp,price
100,100,11.0
200,200,12.5
300,300,10.75
```

The sink writes a `time` column (the graph instant the value was emitted at)
alongside the record's own serde fields.

## See also

- [`lines`](../lines/) — the dependency-free equivalent for plain text.
- [`kdb`](../kdb/) / [`postgres`](../postgres/) — time-sliced historical reads
  from a real store.
- [`core/produce_async_feed`](../../core/produce_async_feed/) — the replay
  machinery underneath, on its own.
