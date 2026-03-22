# KDB+ Read Cached Example

Reads from a KDB+ table using `kdb_read_cached`, which checks a local file cache
before issuing each time-slice query. On the first run all slices are cache misses
and results are fetched from KDB+ and written to disk. On the second run every
slice is a cache hit — no TCP connection to KDB+ is opened.

A 10 MiB cap is configured; oldest files are evicted automatically when the limit
is exceeded. `CacheConfig::clear()` removes all `.cache` files at the end.

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
RUST_LOG=info cargo run --example kdb_read_cached --features kdb
```

## Code

```rust
// serde derives are required by kdb_read_cached
#[derive(Debug, Clone, Default, serde::Serialize, serde::Deserialize)]
struct Price {
    sym: Sym,
    mid: f64,
}

impl KdbDeserialize for Price {
    fn from_kdb_row(
        row: Row<'_>,
        _columns: &[String],
        interner: &mut SymbolInterner,
    ) -> Result<(NanoTime, Self), KdbError> {
        let time = row.get_timestamp(0)?;
        Ok((time, Price {
            sym: row.get_sym(1, interner)?,
            mid: row.get(2)?.get_float()?,
        }))
    }
}

// 10 MiB cap — oldest cache files are evicted when the limit is exceeded.
// Use u64::MAX for an unbounded cache.
let config = CacheConfig::new("/tmp/wingfoil-kdb-cache", 10 * 1024 * 1024);

kdb_read_cached::<Price, _>(
    conn,
    Duration::from_secs(10),
    config,
    |(t0, t1), _date, _iter| {
        format!(
            "select time, sym, mid from prices \
             where time >= (`timestamp$){}j, time < (`timestamp$){}j",
            t0.to_kdb_timestamp(),
            t1.to_kdb_timestamp(),
        )
    },
)
.logged("prices", Info)
.run(
    RunMode::HistoricalFrom(NanoTime::from_kdb_timestamp(0)),
    RunFor::Duration(Duration::from_secs(100)),
)?;

```

## Output

```
Run 1: cache miss — queries KDB and writes cache files
[INFO  prices] (NanoTime(...), Price { sym: AAPL, mid: 150.25 })
[INFO  prices] (NanoTime(...), Price { sym: GOOG, mid: 2800.5 })
[INFO  prices] (NanoTime(...), Price { sym: MSFT, mid: 310.75 })
[INFO  prices] (NanoTime(...), Price { sym: AAPL, mid: 151.0 })
[INFO  prices] (NanoTime(...), Price { sym: GOOG, mid: 2805.0 })
Run 2: cache hit — no KDB connection needed
[INFO  prices] (NanoTime(...), Price { sym: AAPL, mid: 150.25 })
...
Done
```
