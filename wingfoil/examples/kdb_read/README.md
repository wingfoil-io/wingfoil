# KDB+ Read Example

Minimal example: read a table from KDB+ and print each row.

## Setup

Start KDB+ on port 5000 and create a `prices` table with some data:

```q
q -p 5000
```

```q
prices:([]time:`timestamp$();sym:`symbol$();mid:`float$())
`prices insert (2000.01.01D00:00:00.000000000;`AAPL;150.25)
`prices insert (2000.01.01D00:00:01.000000000;`GOOG;2800.50)
`prices insert (2000.01.01D00:00:02.000000000;`MSFT;310.75)
```

## Run

```sh
cargo run --example kdb_read --features kdb
```

## Output

```
AAPL mid=150.2500
GOOG mid=2800.5000
MSFT mid=310.7500
```
