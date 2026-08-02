# KDB+ Read Cached Example (wingfoil-next)

Reads from a KDB+ table using `kdb_read_cached`, which checks a local file cache
before issuing each time-slice query. On the first run all slices are cache
misses and results are fetched from KDB+ and written to disk; on a later run
every slice is a cache hit — no TCP connection to KDB+ is opened. This is the
wingfoil-next port of the classic `kdb_read_cached` example.

A 512 MiB cap is configured; oldest files are evicted automatically when the
limit is exceeded. Delete the folder (or call `CacheConfig::clear()`) to force a
refetch — `bincode` is not schema-evolution safe, so change `T` and you must
clear the cache.

## Setup

Start KDB+ on port 5000 and create a `prices` table:

```sh
q -p 5000
```

```q
prices:([]time:`timestamp$();sym:`symbol$();mid:`float$())
`prices insert (2000.01.01D00:00:05.000000000;`AAPL;150.25)
`prices insert (2000.01.01D00:00:15.000000000;`GOOG;2800.50)
`prices insert (2000.01.01D00:00:25.000000000;`MSFT;310.75)
`prices insert (2000.01.01D00:00:55.000000000;`AAPL;151.00)
`prices insert (2000.01.01D00:01:25.000000000;`GOOG;2805.00)
```

## Run

```sh
RUST_LOG=info cargo run -p wingfoil-next --example kdb_read_cached --features kdb
```

Run it twice — the second run serves every slice from the cache without opening
a KDB connection.

## Code

`kdb_read_cached` is `kdb_read` with a [`CacheConfig`] in place of the
`buffer_size` argument. `T` must additionally implement
`serde::Serialize + Deserialize + Sync`:

```rust
let cache = CacheConfig::new("/tmp/wingfoil-kdb-cache", 512 * 1024 * 1024);

let g = GraphBuilder::new();
let _prices = kdb_read_cached::<Price>(
    &g,
    params,
    conn,
    Duration::from_secs(3600),
    cache,
    |(t0, t1), _date, _iter| {
        format!(
            "select time, sym, mid from prices \
             where time >= (`timestamp$){}j, time < (`timestamp$){}j",
            t0.to_kdb_timestamp(),
            t1.to_kdb_timestamp(),
        )
    },
)?
.logged("prices", log::Level::Info);

let mut runner = g.build();
runner.run(RunMode::HistoricalFrom(start), run_for)?;
```

## Output

```
Run 1: cache miss — queries KDB and writes cache files
[INFO prices] [Price { sym: AAPL, mid: 150.25 }]
[INFO prices] [Price { sym: GOOG, mid: 2800.5 }]
...
Run 2: cache hit — no KDB connection needed
[INFO prices] [Price { sym: AAPL, mid: 150.25 }]
...
```
