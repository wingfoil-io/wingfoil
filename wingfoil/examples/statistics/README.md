# Statistics Example

Demonstrates the `StatisticsOperators` trait — streaming numeric aggregations
that chain onto any `Stream<T: ToPrimitive>` and emit `f64`. It covers
exponential smoothing (`ewma`, `ewma_decay`), cumulative moments
(`average`, `variance`, `std`), count-windowed rolling operators
(`rolling_mean`/`std`/`min`/`max`/`median`), and time-windowed ones
(`rolling_*_over`). Weightable operators take a `Weighting` — `Count` (every
sample equal) or `Time` (each sample weighted by how long it was in effect), so
irregular tick spacing is handled correctly.

## Run

```sh
cargo run --example statistics
```

## Code

```rust
use std::rc::Rc;
use std::time::Duration;
use wingfoil::*;

fn prices() -> Rc<dyn Stream<f64>> {
    ticker(Duration::from_millis(100))
        .count()
        .map(|n: u64| 100.0 + ((n as f64) * 0.6).sin() * 5.0)
}

fn final_value(stat: Rc<dyn Stream<f64>>) -> f64 {
    stat.clone()
        .run(RunMode::HistoricalFrom(NanoTime::ZERO), RunFor::Cycles(30))
        .unwrap();
    stat.peek_value()
}

fn main() {
    // Smoothing
    let _ = prices().ewma(0.2);
    let _ = prices().ewma_decay(Duration::from_millis(300));

    // Cumulative — count vs time weighted
    let _ = prices().average(Weighting::Count);
    let _ = prices().average(Weighting::Time);
    let _ = prices().std(Weighting::Count);

    // Count-windowed (last N samples)
    let _ = prices().rolling_mean(5);
    let _ = prices().rolling_std(5);
    let _ = prices().rolling_median(5);

    // Time-windowed (last N of graph time)
    let win = Duration::from_millis(500);
    let _ = prices().rolling_mean_over(win, Weighting::Time);
    let _ = prices().rolling_std_over(win, Weighting::Count);

    println!("{:.4}", final_value(prices().ewma(0.2)));
}
```

See [`main.rs`](./main.rs) for the full runnable version.

## Output

```
30 samples of a synthetic price series (~100 ± 5):

ewma(0.2)                          = 98.2729
ewma_decay(300ms)                  = 98.2143
average(Count)                     = 100.0289
average(Time)                      = 100.1594
std(Count)                         = 3.6723
rolling_mean(5)                    = 97.0041
rolling_std(5)                     = 2.2035
rolling_min(5)                     = 95.0367
rolling_max(5)                     = 100.5388
rolling_median(5)                  = 96.2451
rolling_mean_over(500ms, Count)    = 98.0453
rolling_mean_over(500ms, Time)     = 98.4054
rolling_std_over(500ms, Count)     = 3.2232
```
