# Statistics Example

Demonstrates the `StatisticsOperators` trait — streaming numeric aggregations
that chain onto any `Stream<T: ToPrimitive>` and emit `f64`. It covers
exponential smoothing (`ewma`, taking an `EwmaSpan` of `PerTick` or `HalfLife`),
cumulative moments (`average`, `variance`, `std`), count-windowed rolling
operators (`rolling_mean`/`std`/`min`/`max`/`median`), and time-windowed ones
(`rolling_*_over`). Every moment operator — cumulative, count-windowed, and
time-windowed — takes a `Weighting`: `Count` (every sample equal) or `Time`
(each sample weighted by how long it was in effect), so irregular tick spacing
is handled correctly.

One synthetic price stream feeds a spread of statistics, which are `combine`d
into a row, sampled twice a second, and printed live as a table with `for_each`.

## Run

```sh
cargo run --example statistics
```

## Code

```rust
use std::rc::Rc;
use std::time::Duration;
use wingfoil::adapters::statistics::*;
use wingfoil::*;

fn main() {
    let price = ticker(Duration::from_millis(100))
        .count()
        .map(|n: u64| 100.0 + ((n as f64) * 0.6).sin() * 5.0);

    // One statistic per column, all derived from the shared price stream.
    let labels = ["price", "ewma", "sma", "std", "min", "max", "twap"];
    let columns: Vec<Rc<dyn Stream<f64>>> = vec![
        price.clone(),
        price.ewma(EwmaSpan::PerTick(0.3)),
        price.rolling_mean(10, Weighting::Count),
        price.rolling_std(10, Weighting::Count),
        price.rolling_min(10),
        price.rolling_max(10),
        price.average(Weighting::Time),
    ];

    // Header first, then stream: `combine` collects the columns (in order) into
    // one Stream<Burst<f64>> whose value each tick is a row holding every
    // column's latest value. Sample that row twice a second and print it live.
    print!("{:>6}", "time");
    for label in labels {
        print!(" {label:>7}");
    }
    println!();

    let trigger = ticker(Duration::from_millis(500));
    let table = combine(columns).sample(trigger).for_each(|row, time| {
        print!("{:>5.1}s", f64::from(time) / 1e9);
        for value in &row {
            print!(" {value:>7.2}");
        }
        println!();
    });

    Graph::new(vec![table], RunMode::HistoricalFrom(NanoTime::ZERO), RunFor::Duration(Duration::from_secs(5)))
        .run()
        .unwrap();
}
```

See [`main.rs`](./main.rs) for the full runnable version.

## Output

```
  time   price    ewma     sma     std     min     max    twap
  0.0s  102.82  102.82  102.82    0.00  102.82  102.82  102.82
  0.5s   97.79  101.29  102.37    2.70   97.79  104.87  103.29
  1.0s  101.56   99.00   99.84    3.73   95.02  104.87   99.96
  1.5s   99.13  101.43  100.14    3.75   95.02  104.99  101.10
  2.0s  100.17   98.29   99.89    3.78   95.10  104.99  100.00
  2.5s  100.54  101.97  100.08    3.80   95.10  104.83  100.63
  3.0s   98.77   97.82   99.95    3.81   95.04  104.83  100.03
  3.5s  101.91  102.36  100.01    3.82   95.04  105.00  100.42
  4.0s   97.46   97.51  100.02    3.82   95.07  105.00  100.06
  4.5s  103.12  102.57   99.94    3.81   95.07  104.78  100.29
  5.0s   96.36   97.41  100.09    3.79   95.06  104.78  100.09
```
