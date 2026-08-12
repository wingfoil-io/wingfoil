## Order book: real market data, in and out

A CSV of NASDAQ limit orders for AAPL goes in; a limit order book is maintained
over it, and trades and two-way prices come back out as two CSV files. The data
is a sample from [lobsterdata](https://lobsterdata.com/info/DataSamples.php),
and the book is maintained by the coincidentally named
[lobster](https://github.com/rubik/lobster) crate. LOBSTER's own readme —
the attribution for `data/aapl.csv` and the definition of its message columns —
is kept beside the data as [`data/aapl_readme.txt`](data/aapl_readme.txt).

**Every stream here runs at a different frequency.** Messages arrive in bursts
sharing a timestamp; the book's top changes less often than that; trades are
sparser still. Nothing is coalesced or dropped to make those rates agree — the
graph clock is driven by the data's own timestamps, so each stream ticks when it
has something to say.

```rust,ignore
let book = RefCell::new(lobster::OrderBook::default());
let get_time = |msg: &Message| NanoTime::new((msg.seconds * 1e9) as u64);

let g = GraphBuilder::new();
let (fills, prices) = csv_read(&g, &source_path, get_time, true, None)?
    .map(move |chunk: &Burst<Message>| process_orders(chunk, &book))
    .split();

let _prices_sink = prices.filter_none().distinct().csv_write(&prices_path)?;
let _fills_sink = fills.csv_write(&fills_path)?;

g.build().run(RunMode::HistoricalFrom(NanoTime::ZERO), RunFor::Forever)?;
```

One node maintains the book and emits a `(fills, price)` pair; `split`
decomposes that into the two output streams. `filter_none` drops the cycles that
produced no price, `distinct` emits only on change, and each `csv_write` is a
sink driven by the graph clock.

## Run

```sh
cargo run --release --manifest-path crates/wingfoil/Cargo.toml \
    --features csv --example order_book
```

```text
replayed examples/core/order_book/data/aapl.csv
  15040 two-way prices -> examples/core/order_book/data/prices.csv
  4169 fills          -> examples/core/order_book/data/fills.csv
in 99.423ms
```

An hour of market data — 91,998 messages — in about a tenth of a second.

## Plot

`plot.py` draws the two outputs together: the two-way prices as step lines, the
fills as points sized by quantity.

```sh
pip install pandas matplotlib
python3 crates/wingfoil/examples/core/order_book/plot.py
```

<div align="center">
  <img alt="AAPL best bid/ask with fills overlaid" src="aapl.svg"/>
</div>

## Parity

This is a port of the legacy engine's `order_book` example, and its two output
files are **byte-identical** to the ones the legacy engine produces from the same
input — same values, same timestamps. The chart above is the legacy engine's,
and it is equally a chart of this one.
