# PostgreSQL Adapter Example (wingfoil)

Round-trips data through PostgreSQL: generates trades, writes them with
`postgres_write`, reads them back with the time-sliced `postgres_read`, and
asserts the two tie out. A port of the legacy `legacy/wingfoil/examples/postgres`
example onto the next engine. Demonstrates the on-graph time model
(`(NanoTime, T)` tuples) and the shared time-slicing logic used for historical
replay.

The graph owns the tokio runtime the adapters use — created lazily, so no
`&Handle` is threaded in (see the module docs). Because the reader and sink drive
the async client with `Handle::block_on`, each graph is built, run, and dropped
from the (non-async) main thread.

## Setup

```sh
docker run --rm -p 5432:5432 -e POSTGRES_PASSWORD=postgres postgres:16-alpine
```

## Run

```sh
cargo run --manifest-path crates/wingfoil/Cargo.toml --example postgres_adapter --features postgres
```

## Code

```rust
let start = NanoTime::from_kdb_timestamp(0);

// Write — replay generated trades at their timestamps into the table.
{
    let g = GraphBuilder::new(); // owns the tokio runtime lazily
    let rows = baseline.iter().cloned().map(|(time, trade)| Ok((trade, time)));
    let _sink = g
        .replay_results(rows)
        .postgres_write(conn.clone(), "example_trades", None)?;
    g.build().run(run_mode, run_for)?;
}

// Read — time-sliced, one query per day (a single 24h slice covers the run).
let params = RunParams { run_mode, run_for, start_time: start };
let read = postgres_read::<Trade>(&g, params, conn,
    Duration::from_secs(86400),
    |(t0, t1), _date, _iter| format!(
        "SELECT time, sym, price, qty FROM example_trades \
         WHERE time >= '{}' AND time < '{}' ORDER BY time",
        postgres_timestamp(t0), postgres_timestamp(t1),
    ),
)?;
```

See [`main.rs`](./main.rs) for the full listing.

## Output

```
✓ 10 written, read and validated
```
