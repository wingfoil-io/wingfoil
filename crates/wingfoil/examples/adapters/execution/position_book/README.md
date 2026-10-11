# Execution Position Book Example (wingfoil)

A `Book` valued by the caller's `Measure`, as a node in a graph: linear equity PnL, an index future at $50 a point, an inverse future's convexity, a cashflow, an unvalued variance swap, and totals one currency at a time.

## Prerequisites

None — offline and instant.

## Run

```sh
cargo run -p wingfoil --example execution_position_book --features execution
```

## Code

`Book::new` is handed how each instrument makes money. The book is node state: the example's `BookOp` applies each instant's fills and then its cashflows in order and emits the book, and a three-line `BookOps` trait is the fluent form — the shape a library node takes. Fills, cashflows and marks are replayed at engine instants; each fold prints what is held, and each mark prints the valuation it finds, with `pnl` / `total` answering `None` rather than a zero where nothing values a contract. See [`main.rs`](main.rs).

## Output

```text
t=1 book:
  Aapl: net 100, entry 190, realised 0, fees 1, cashflows 0
  unvalued fills: 0
t=2 book:
  Aapl: net 50, entry 190, realised 250, fees 1.5, cashflows 0
  unvalued fills: 0
t=3 book:
  Aapl: net 50, entry 190, realised 250, fees 1.5, cashflows 0
  Es: net 2, entry 5000, realised 0, fees 4, cashflows 0
  BtcInverse: net 60000, entry 60000, realised 0, fees 0.00001, cashflows 0
  VarSwap: net 10, entry -, realised 0, fees 0, cashflows 0
  unvalued fills: 1
t=4 book:
  Aapl: net 50, entry 190, realised 250, fees 1.5, cashflows 0
  Es: net 2, entry 5000, realised 0, fees 4, cashflows 0
  BtcInverse: net 60000, entry 60000, realised 0, fees 0.00001, cashflows -0.00002
  VarSwap: net 10, entry -, realised 0, fees 0, cashflows 0
  unvalued fills: 1
t=5 marked:
  Aapl @ 200: unrealised 500
  Es @ 5001.25: unrealised 125
  BtcInverse @ 66000: unrealised 0.090909091
  VarSwap @ 21: unrealised none (no measure)
  Usd total: none — an open position cannot be valued
  Btc total: net 0.090879091 (realised 0, unrealised 0.090909091, fees 0.00001, cashflows -0.00002)
t=6 marked:
  Aapl @ 200: unrealised 500
  Es @ 5001.25: unrealised 125
  BtcInverse @ 54000: unrealised -0.111111111
  VarSwap @ 21: unrealised none (no measure)
  Usd total: none — an open position cannot be valued
  Btc total: net -0.111141111 (realised 0, unrealised -0.111111111, fees 0.00001, cashflows -0.00002)
t=7 marked:
  Aapl @ 200: unrealised 500
  Es @ 5001.25: unrealised 125
  BtcInverse @ 63000: unrealised 0.047619048
  VarSwap @ 21: unrealised none (no measure)
  Usd total: none — an open position cannot be valued
  Btc total: net 0.047589048 (realised 0, unrealised 0.047619048, fees 0.00001, cashflows -0.00002)
```
