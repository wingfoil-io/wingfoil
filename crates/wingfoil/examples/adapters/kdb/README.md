# KDB+ Adapter Examples (wingfoil)

KDB+ integration, in three examples that build on each other. Ports of the
classic `wingfoil/examples/kdb/*`.

| Example | What it shows |
|---|---|
| [`read`](read/) | Time-sliced reads — a query issued per time slice over a window, streamed onto the graph clock. |
| [`read_cached`](read_cached/) | The same, behind an LRU **file cache**: slices already on disk are not re-queried. |
| [`round_trip`](round_trip/) | The full loop — write to KDB+, read it back, validate what came out. |

Read them in that order. `read` establishes the time-slice model, `read_cached`
adds the caching layer on top of it, and `round_trip` closes the write side.

## Prerequisites

All three require a running KDB+ / tickerplant instance. Each example's own
README gives the connection details and the table it expects.

## Run

```sh
cargo run -p wingfoil --example kdb_read         --features kdb
cargo run -p wingfoil --example kdb_read_cached  --features kdb
cargo run -p wingfoil --example kdb_round_trip   --features kdb
```

## The time-slice model

The KDB+ source does not stream a table — it issues **one query per time slice**
and emits the rows as a burst at the slice's graph instant. You supply the slice
width and a closure that builds the query for a given `(t0, t1)`:

```rust,ignore
kdb_read::<Price>(
    conn,
    Duration::from_secs(10),
    |(t0, t1), _date, _iter| {
        format!(
            "select time, sym, mid from prices \
             where time >= (`timestamp$){}j, time < (`timestamp$){}j",
            t0.to_kdb_timestamp(),
            t1.to_kdb_timestamp(),
        )
    },
    ..
)
```

That keeps a multi-day backtest from materialising a whole table in memory, and
it is what makes the historical replay honour the data's own timestamps. Rows
are typed: implement `KdbDeserialize` for your struct to map columns onto fields.

## See also

- [`postgres`](../postgres/) — the same time-sliced-read and streaming-write
  shape against PostgreSQL.
- [`csv`](../csv/) — the same replay model with no server at all.
