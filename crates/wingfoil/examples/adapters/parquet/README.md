# Parquet Adapter Example (wingfoil)

The Parquet adapter end to end: archive a stream into a Hive-partitioned tree of
`.parquet` files — one directory per UTC day — then replay the whole tree back
as one stream, each row at the graph time that wrote it.

Parquet is the compact **archive and backtest** format: rows are buffered into
compressed row groups (snappy by default) and each file's footer is written when
the sink moves to the next partition or the run ends. For a capture that must be
durable tick by tick, write [Arrow IPC](../arrow/) instead and compact it later —
compaction is just a graph, `arrow_read(capture)` into
`parquet_write_partitioned(archive, TimePartition::Day)`.

## Run

No prerequisites — the example writes its tree under the OS temp directory.

```sh
cargo run -p wingfoil --example parquet_adapter --features parquet
```

## Code

Records are plain serde structs; their field names become the Parquet columns.
The archive runs a historical ticker from 2026-10-02T04:00Z, one trade every
eight hours, into a tree partitioned by day:

```rust
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
struct Trade {
    seq: u64,
    px: f64,
    qty: u32,
}

let g = GraphBuilder::new();
let trades = g
    .ticker(Duration::from_secs(8 * 3600))
    .count()
    .map(|&n| Trade { seq: n, px: 100.0 + n as f64 * 0.25, qty: 10 * n as u32 });
let _sink = trades.parquet_write_partitioned(&root, TimePartition::Day)?;
g.build().run(RunMode::HistoricalFrom(start), RunFor::Cycles(7))?;
```

Reading back takes the **root directory**: every `*.parquet` file under it is
replayed in path order — which, zero-padded, is time order — as a single
stream. The sink wrote a leading `time` column (a nanosecond `Timestamp`), and
naming it in the read type as an `i64` lets the replay run at the original
graph times:

```rust
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
struct Archived {
    time: i64,
    seq: u64,
    px: f64,
    qty: u32,
}

let g = GraphBuilder::new();
let _print = parquet_read(&g, &root, |a: &Archived| NanoTime::new(a.time as u64))?
    .with_time()
    .for_each(|(time, burst)| {
        for a in burst.iter() {
            println!("{}  seq={} px={:.2} qty={}", NaiveDateTime::from(*time), a.seq, a.px, a.qty);
        }
        Ok(())
    });
g.build().run(RunMode::HistoricalFrom(NanoTime::ZERO), RunFor::Forever)?;
```

Three things to note:

- **A file is readable once it is finished, not before.** The footer is written
  when the sink leaves a partition (graph time only moves forward) or at
  teardown — including after an aborted run. In a live run every day before
  today is already a complete file.
- **The reader is lazy and bounded.** It reads the footer at wiring, then one
  row group and one decoded batch at a time, and runs at most `buffer_size`
  timestamp-groups ahead of the graph (`ParquetReadOptions`, default
  `Some(1024)`), so a tree of any size replays in bounded memory.
- **Other tools read the tree as-is.** The `key=value` directories are Hive
  partitioning, which DuckDB, Polars, pyarrow and Spark discover natively. In
  DuckDB, for example:

  ```sql
  SELECT day, count(*) AS trades, avg(px) AS avg_px
  FROM read_parquet('/tmp/wingfoil_parquet_adapter/**/*.parquet', hive_partitioning = true)
  GROUP BY day ORDER BY day;
  ```

  prints (via DuckDB's Python client):

  ```text
  ┌─────────┬────────┬────────┐
  │   day   │ trades │ avg_px │
  │ varchar │ int64  │ double │
  ├─────────┼────────┼────────┤
  │ 02      │      3 │  100.5 │
  │ 03      │      3 │ 101.25 │
  │ 04      │      1 │ 101.75 │
  └─────────┴────────┴────────┘
  ```

## Output

```text
wrote /tmp/wingfoil_parquet_adapter:
  year=2026/month=10/day=02/data.parquet
  year=2026/month=10/day=03/data.parquet
  year=2026/month=10/day=04/data.parquet
replayed:
2026-10-02 04:00:00  seq=1 px=100.25 qty=10
2026-10-02 12:00:00  seq=2 px=100.50 qty=20
2026-10-02 20:00:00  seq=3 px=100.75 qty=30
2026-10-03 04:00:00  seq=4 px=101.00 qty=40
2026-10-03 12:00:00  seq=5 px=101.25 qty=50
2026-10-03 20:00:00  seq=6 px=101.50 qty=60
2026-10-04 04:00:00  seq=7 px=101.75 qty=70
```

The first line is the OS temp directory, so it differs by platform.

## See also

- [`arrow`](../arrow/) — the live-capture format on the same columnar core:
  flushed every tick, readable while written.
- [`csv`](../csv/) — the row-oriented, text cousin: same record types, same
  lazy replay machinery.
- [`core/async`](../../core/async/) — the replay machinery underneath
  (`produce_async`), on its own.
