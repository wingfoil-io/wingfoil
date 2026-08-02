## RunMode Example

- Demonstrates how to swap between `RunMode::RealTime` and `RunMode::HistoricalFrom` using a builder trait
- Graph wiring is identical in both modes — only the builder changes

The `MarketDataBuilder` trait defines a `price()` method returning a stream of prices.
`RealTimeMarketDataBuilder` and `HistoricalMarketDataBuilder` both rely on the default
mock implementation, which emits synthetic prices cycling through 100.0 – 109.0.
In a real system each struct would override `price()` with its own data source.

```bash
cargo run --example run_mode -- historical
cargo run --example run_mode -- realtime
```

```rust
trait MarketDataBuilder {
    fn price(&self) -> Rc<dyn Stream<f64>> {
        ticker(Duration::from_nanos(1))
            .count()
            .map(|n: u64| 100.0 + (n % 10) as f64)
    }
}

struct RealTimeMarketDataBuilder;
struct HistoricalMarketDataBuilder;

impl MarketDataBuilder for RealTimeMarketDataBuilder {}
impl MarketDataBuilder for HistoricalMarketDataBuilder {}

fn run(run_mode: RunMode) {
    let builder: Box<dyn MarketDataBuilder> = match run_mode {
        RunMode::RealTime => Box::new(RealTimeMarketDataBuilder),
        RunMode::HistoricalFrom(_) => Box::new(HistoricalMarketDataBuilder),
    };

    // Build the graph — add business logic here.
    let prices = builder.price();

    prices.run(run_mode, RunFor::Cycles(5)).unwrap();
    println!("last price: {}", prices.peek_value());
}
```
