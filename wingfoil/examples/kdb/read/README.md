# KDB+ Read Example

Reads from a KDB+ table using 10-second time slices over a 100-second window.
Each chunk issues a separate time-bounded query, demonstrating the time slicing feature.

## Setup

Start KDB+ on port 5000 and create a `prices` table with data spread across multiple 10s chunks:

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
RUST_LOG=info cargo run --example kdb_read --features kdb
```

## Output

```
[INFO prices] Price { sym: AAPL, mid: 150.25 }
[INFO prices] Price { sym: GOOG, mid: 2800.5 }
[INFO prices] Price { sym: MSFT, mid: 310.75 }
[INFO prices] Price { sym: AAPL, mid: 151.0 }
[INFO prices] Price { sym: GOOG, mid: 2805.0 }
```
