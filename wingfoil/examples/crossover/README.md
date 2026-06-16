## Crossover Example

A moving-average **crossover** trading signal, built entirely from core
wingfoil operators — no I/O adapters required. Because it is small and uses
only the fluent API, it makes a great from-scratch, live-coded walkthrough.

A synthetic mid price feeds two exponential moving averages (EMAs):

- a **fast** EMA (`alpha = 0.30`) that reacts quickly to price moves, and
- a **slow** EMA (`alpha = 0.05`) that lags behind.

When the fast EMA is above the slow EMA the signal is `LONG`; otherwise `FLAT`.
The classic "golden cross / death cross" strategy.

```bash
# Watch it tick on the wall clock (real time):
cargo run --example crossover
```

The graph wiring is the interesting part:

```rust
let prices = ticker(period)
    .count()
    .map(|n| 100.0 + 15.0 * ((n as f64) / 25.0).sin() + (n as f64) * 0.02);

let fast = prices.fold(ema(0.30)); // reacts quickly
let slow = prices.fold(ema(0.05)); // lags behind

let report = trimap(
    Dep::Active(prices.clone()),
    Dep::Passive(fast.clone()),
    Dep::Passive(slow.clone()),
    |price, fast, slow| {
        let signal = if fast > slow { "^ LONG" } else { "v FLAT" };
        format!("price {price:7.2}   fast {fast:7.2}   slow {slow:7.2}   {signal}")
    },
);

report
    .for_each(|line, time| println!("{}   {line}", time.pretty()))
    .run(RunMode::RealTime, RunFor::Cycles(150))?;
```

Sample output:

```pre
... price  100.62   fast  100.62   slow  100.62   v FLAT
... price  101.24   fast  100.81   slow  100.65   ^ LONG
... price  112.14   fast  112.92   slow  112.95   v FLAT
... price   91.52   fast   90.67   slow   90.56   ^ LONG
```

Two things worth pointing out in a demo:

- `fold` carries state (the running EMA) from one tick to the next — this is
  how you build stateful operators on top of a stream.
- `for_each` prints immediately, so output streams live. `print()` buffers and
  flushes on drop, which is great for tests but not for a live demo.

To turn the same graph into an **instant backtest**, change the single line:

```rust
.run(RunMode::HistoricalFrom(NanoTime::ZERO), RunFor::Cycles(150))?;
```

The wiring is unchanged — only the run mode differs. This is the same property
the [`run_mode`](../run_mode/) and [`order_book`](../order_book/) examples rely
on for backtesting.
