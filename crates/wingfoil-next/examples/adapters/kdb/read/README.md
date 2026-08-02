# KDB+ Read Example (wingfoil-next)

Reads from a KDB+ table using 10-second time slices over a 100-second window.
Each slice issues a separate time-bounded query, demonstrating the time-slicing
feature. This is the wingfoil-next port of the legacy `kdb_read` example.

## Setup

Start KDB+ on port 5000 and create a `prices` table with data spread across
multiple 10s slices:

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
RUST_LOG=info cargo run -p wingfoil-next --example kdb_read --features kdb
```

## Code

The reader is a free function taking the `GraphBuilder` and the run's
[`RunParams`] (it needs the run window to slice queries at wiring — a pure
check); the connect and slice queries then run at the start of the run:

```rust
let g = GraphBuilder::new();
let _prices = kdb_read::<Price>(
    &g,
    params, // RunParams describing the historical run
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
    None, // buffer_size (no-op in historical mode)
)?
.logged("prices", log::Level::Info);

let mut runner = g.build();
runner.run(RunMode::HistoricalFrom(start), run_for)?;
```

## Output

```
[INFO prices] [Price { sym: AAPL, mid: 150.25 }]
[INFO prices] [Price { sym: GOOG, mid: 2800.5 }]
[INFO prices] [Price { sym: MSFT, mid: 310.75 }]
[INFO prices] [Price { sym: AAPL, mid: 151.0 }]
[INFO prices] [Price { sym: GOOG, mid: 2805.0 }]
```
