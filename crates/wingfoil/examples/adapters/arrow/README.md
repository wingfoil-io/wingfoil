# Arrow Adapter Example (wingfoil)

The Arrow IPC adapter end to end: capture a stream into a Hive-partitioned tree
of `.arrows` files — one directory per UTC day — then replay the whole tree back
as one stream, each row at the graph time that wrote it.

Arrow IPC is the durable **live-capture** format: the sink flushes every tick's
rows to the OS as one self-contained record batch, so a file is readable while
it is still being written and a crash loses at most the tick in flight. The
partitioned layout is the one Hive, Spark, DuckDB, pyarrow and pandas discover
natively.

## Run

No prerequisites — the example writes its tree under the OS temp directory.

```sh
cargo run -p wingfoil --example arrow_adapter --features arrow
```

## Code

Records are plain serde structs; their field names become the Arrow columns. The
capture runs a historical ticker from 2026-10-02T04:00Z, one trade every eight
hours, into a tree partitioned by day:

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
let _sink = trades.arrow_write_partitioned(&root, TimePartition::Day)?;
g.build().run(RunMode::HistoricalFrom(start), RunFor::Cycles(7))?;
```

Reading back takes the **root directory**: every `*.arrows` file under it is
replayed in path order — which, zero-padded, is time order — as a single
stream. The sink wrote a leading `time` column (a nanosecond Arrow
`Timestamp`), and naming it in the read type as an `i64` lets the replay run at
the original graph times:

```rust
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
struct Captured {
    time: i64,
    seq: u64,
    px: f64,
    qty: u32,
}

let g = GraphBuilder::new();
let _print = arrow_read(&g, &root, |c: &Captured| NanoTime::new(c.time as u64))?
    .with_time()
    .for_each(|(time, burst)| {
        for c in burst.iter() {
            println!("{}  seq={} px={:.2} qty={}", NaiveDateTime::from(*time), c.seq, c.px, c.qty);
        }
        Ok(())
    });
g.build().run(RunMode::HistoricalFrom(NanoTime::ZERO), RunFor::Forever)?;
```

Three things to note:

- **One file open at a time.** Graph time only moves forward, so when a row
  lands in a new day the sink finishes the previous day's file and opens the
  next. A day with no rows gets no directory.
- **The reader is lazy and bounded.** It reads one record batch at a time and
  runs at most `buffer_size` timestamp-groups ahead of the graph
  (`ArrowReadOptions`, default `Some(1024)`), so a tree of any size replays in
  bounded memory.
- **Columns the read type doesn't name are ignored.** Drop `time` from
  `Captured` and the same files read back fine — you would just stamp the rows
  from a field of your own.

## Output

```text
wrote /tmp/wingfoil_arrow_adapter:
  year=2026/month=10/day=02/data.arrows
  year=2026/month=10/day=03/data.arrows
  year=2026/month=10/day=04/data.arrows
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

- [`csv`](../csv/) — the row-oriented, text cousin: same record types, same
  lazy replay machinery.
- [`lines`](../lines/) — the dependency-free equivalent for plain text.
- [`core/async`](../../core/async/) — the replay machinery underneath
  (`produce_async`), on its own.
