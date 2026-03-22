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
Clearing cache
Done
```
