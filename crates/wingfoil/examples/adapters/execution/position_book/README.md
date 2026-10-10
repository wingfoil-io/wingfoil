# Execution Position Book Example (wingfoil)

A `Book` valued by the caller's `Measure`: linear equity PnL, an index future at $50 a point, an inverse future's convexity, a cashflow, an unvalued variance swap, and totals one currency at a time.

## Prerequisites

None — offline and instant.

## Run

```sh
cargo run -p wingfoil --example execution_position_book --features execution
```

## Code

`Book::new` is handed how each instrument makes money; fills are folded with `Book::apply`, and `pnl` / `total` answer `None` rather than a zero where nothing values a contract. See [`main.rs`](main.rs).

## Output

```text
AAPL: net 50, entry Some(Px(190000000000)), realised $250
BTC inverse: +10% → 0.090909091 BTC, −10% → -0.111111111 BTC
ES: net 2, entry Some(Px(5000000000000)), +1.25 points → $125
variance swap: net 10, pnl None, unvalued fills 1
BTC total: net 0.047589048 (cashflows -0.00002, fees 0.00001)
USD total: None — the variance swap cannot be valued, so no total
  Aapl: 50
  Es: 2
  BtcInverse: 60000
  VarSwap: 10
```
