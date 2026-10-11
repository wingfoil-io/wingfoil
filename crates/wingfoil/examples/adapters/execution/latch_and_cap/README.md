# Execution Latch And Cap Example (wingfoil)

The kill switch over limits of your own, as a node in a graph — latch, `clear` refusing, `NaN` outside the cap, a restart from a `Snapshot` — and the per-order `Cap` judged by `ceiling::Capped` on the request wire.

## Prerequisites

None — offline and instant.

## Run

```sh
cargo run -p wingfoil --example execution_latch_and_cap --features execution
```

## Code

`Switch` owns the latch over a caller-defined `Limit` set; the caps and what they compare stay the caller's. The switch is node state in the example's `SwitchOp`: the book's readings are assessed and recorded first, then the operator's `clear`, which refuses while a breach stands. The restart is a second graph started from the first run's `Snapshot`: still halted, until a reading finds the book inside and the operator clears it. The ceiling is `Capped` around a venue on the request wire: the first order over its cap aborts the run, and the error names the order. See [`main.rs`](main.rs).

## Output

```text
the day:
t=  0s Open: standing {}
t= 60s Halted: standing {Gross, DailyLoss}, latched {Gross, DailyLoss} since t=60s
t=120s Halted: standing {}, latched {Gross, DailyLoss} since t=60s
t=180s Halted: standing {Gross, Stale}, latched {Gross, DailyLoss, Stale} since t=60s; clear → Err(StillBreached({Gross, Stale}))
  carried: latched {Gross, DailyLoss, Stale} since t=60s, standing {Gross, Stale}, opening equity 1000000
restarted from the snapshot:
t=240s Halted: standing {}, latched {Gross, DailyLoss, Stale} since t=60s
t=245s Open: standing {}; clear → Ok(())
the ceiling:
t=  1s order 1 acked
  the run aborted: node 1 (TryMapFilter) cycle: order ClientOrderId(2) for 501 on Aapl is over the ceiling of 500
```
