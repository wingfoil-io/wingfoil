# Execution Latch and Cap Example (wingfoil)

The kill switch over limits of your own — latch, `clear` refusing, `NaN` outside the cap, a restart — and the per-order `Cap` judged by `Judge`.

## Prerequisites

None — offline and instant.

## Run

```sh
cargo run -p wingfoil --example execution_latch_and_cap --features execution
```

## Code

`Switch` owns the latch over a caller-defined `Limit` set; the caps and what they compare stay the caller's. The ceiling judges each order against a per-contract cap before a venue sees it. See [`main.rs`](main.rs).

## Output

```text
open:  Open, day opened at Some(Qty(1000000000000000))
gap:   Halted, standing {Gross, DailyLoss}
after: Halted, latched {Gross, DailyLoss} since Some(NanoTime(1728048660000000000))
nan:   standing {Gross, Stale}; clear → Err(StillBreached({Gross, Stale}))
restart: still Halted
clear: Open
500 AAPL → Ok(())
501 AAPL → Err(Over { order: ClientOrderId(1), instrument: Aapl, qty: Qty(501000000000), ceiling: Qty(500000000000) })
1 PENNY → refused, nothing bounds it
```
