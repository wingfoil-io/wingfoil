

## Order Book Example

In this example, we load a CSV file of limit orders
for AAPL stock ticker trading on the NASDAQ exchange.
The data is sourced from
[lobsterdata samples](https://lobsterdata.com/info/DataSamples.php)

We use the coincidentally named [lobster](https://github.com/rubik/lobster)
rust library to maintain an order book over time.

Trades and two-way prices are derived, exported back out to CSV
and plotted below.

The frequencies of the inputs and outputs are all different to each other.

<div align="center">
  <img alt="diagram" src="https://raw.githubusercontent.com/wingfoil-io/wingfoil/refs/heads/main/wingfoil/diagrams/aapl.svg"/>
</div>

In addition to the csv output, we also get a performance summary and
pretty-print of the graph.  One hours worth of data was processed
in 287 milliseconds.
```pre
8 nodes wired in 10.326µs
Completed 91998 cycles in 287.125397ms. 3.12µs average.
[00] CsvReaderStream
[01]   FilterStream
[02]      MapStream
[03]        MapStream
[04]          DistinctStream
[05]            CsvWriterNode
[06]        MapStream
[07]          CsvVecWriterNode
```

<div style="page-break-after: always;"></div>

```rust, ignore

pub fn main() {
    env_logger::init();
    let book = RefCell::new(lobster::OrderBook::default());
    let source_path = "examples/lobster/data/aapl.csv";
    let fills_path = "examples/lobster/data/fills.csv";
    let prices_path = "examples/lobster/data/prices.csv";
    // map from seconds from midnight to NanoTime time
    let get_time = |msg: &Message| NanoTime::new((msg.seconds * 1e9) as u64);
    let (fills, prices) = csv_read_vec(source_path, get_time, true)
        .map(move |chunk| process_orders(chunk, &book))
        .split();
    let prices_export = prices
        .filter_value(|price: &Option<TwoWayPrice>| !price.is_none())
        .map(|price| price.unwrap())
        .distinct()
        .csv_write(prices_path);
    let fills_export = fills.csv_write_vec(fills_path);
    let run_mode = RunMode::HistoricalFrom(NanoTime::ZERO);
    let run_for = RunFor::Forever;
    Graph::new(vec![prices_export, fills_export], run_mode, run_for)
        .print()
        .run()
        .unwrap();
}
```
