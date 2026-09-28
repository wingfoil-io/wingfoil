# Polars Adapter Example (wingfoil)

The polars adapter end to end: replay a Parquet file of quotes as a
deterministic historical burst stream, derive a mid price per row, and collect
the result back into a `DataFrame` and an Arrow IPC file.

## Prerequisites

None — the example builds its own input frame and stages it as a Parquet file in
the OS temp directory.

## Run

```sh
cargo run -p wingfoil --example polars_adapter --features polars
```

## Code

The input frame's `time` column is a microsecond `Datetime`; `polars_read`
names it as the time column, so it becomes the graph clock (scaled to
nanoseconds) and is dropped from the rows themselves:

```rust
let g = GraphBuilder::new();
let quotes = polars_read(&g, &input, "time", None)?;   // Stream<Burst<PolarsRow>>

let schema = Arc::new(Schema::from_iter([
    ("sym".into(), DataType::String),
    ("mid".into(), DataType::Float64),
]));
let mids = quotes.try_map(move |burst: &Burst<PolarsRow>| {
    burst
        .iter()
        .map(|q| {
            let bid: f64 = field(q, "bid")?.try_extract()?;
            let ask: f64 = field(q, "ask")?.try_extract()?;
            let sym = field(q, "sym")?.clone();
            PolarsRow::new(schema.clone(), vec![sym, AnyValue::Float64((bid + ask) / 2.0)])
        })
        .collect::<anyhow::Result<Burst<PolarsRow>>>()
});

let (_collect, collected) = mids.polars_collect();   // DataFrame after the run
let _write = mids.polars_write(&output)?;            // .arrow -> Arrow IPC
```

Three things to note:

- **Same-instant rows ride one burst.** The two quotes at 2µs arrive together
  as one `Burst<PolarsRow>` — never coalesced, never split across cycles.
- **Build the output schema once.** Every output row shares the one
  `Arc<Schema>`, so a clone is a pointer copy and the sink's per-row schema
  check is a pointer comparison.
- **The sinks build the frame at the end of the run.** Rows are buffered and
  the `DataFrame` (with a leading `Datetime[ns]` `time` column of graph times)
  is built once at the end of the run — a columnar file is written once, not
  appended row by row.

## Output

```text
t= 1000ns  "AAPL" 100.1
t= 2000ns  "AAPL" 100.6 | "MSFT" 250.2
t= 3000ns  "AAPL" 101.2

collected 4 rows [time: datetime[ns], sym: str, mid: f64]
wrote 4 rows to /tmp/wingfoil_polars_adapter_mids.arrow
```

## See also

- [`csv`](../csv/) — the same replay machinery over serde-typed CSV rows.
- [`lines`](../lines/) — the dependency-free equivalent for plain text.
