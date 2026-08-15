## Top of book: one graph, replayed and live

Real NASDAQ limit-order messages for AAPL go in; a limit order book is
maintained over them, and a two-way quote comes back out every time either side
moves. The data is the same [lobsterdata](https://lobsterdata.com/info/DataSamples.php)
sample the [`order_book`](../order_book/) example replays, and the book is
maintained by the coincidentally named [lobster](https://github.com/rubik/lobster)
crate.

The point of *this* example is the run mode. The same graph runs as a
deterministic replay and as a live feed:

```sh
cargo run --manifest-path crates/wingfoil/Cargo.toml --example top_of_book
cargo run --manifest-path crates/wingfoil/Cargo.toml --example top_of_book -- realtime
```

Both print the same quotes in the same order. Only the clock differs — and
there is no second implementation to drift.

```rust
// The only line that differs between backtest and live.
let feed = market_data(run_mode)?.connect(&g)?;

// The apex: one node maintains the book.
let top = feed.messages.map(move |burst| apply(burst, &book));

// Each side moves at its own rate.
let bid = top.map(|t| t.bid).distinct();
let ask = top.map(|t| t.ask).distinct();

// The recombine: fires when either side moves.
bid.join(&ask, quote)
    .filter_none()
    .distinct()
    .with_time()
    .for_each(print_quote);
```

### The diamond, and why it matters here

`top` is a **shared apex**: both branches read it, and the engine runs it once
per cycle no matter how many readers it has. That is not a nicety — the node
maintains a limit order book, and rebuilding it once per downstream path would
be both wrong and expensive.

`bid` and `ask` then run at **different frequencies**. Most messages move
neither side of the top; many move only one. `distinct` reduces each branch to
the cycles where that side actually changed, and `join` fires when *either*
does, which is exactly when the quote changed. Nothing is coalesced or dropped
to make those rates agree.

### The `MarketData` seam

Where the data comes from is a trait, not a branch scattered through the
program:

```rust
trait MarketData {
    fn connect(&self, g: &GraphBuilder) -> anyhow::Result<Feed>;
}
```

Two implementations, and `market_data(run_mode)` picks one — the **only** place
the program branches on run mode:

- **`Replay`** stamps each message with its own time (`send_at`) and lets the
  engine schedule it on the graph clock, consulting no wall clock at all.
  Deterministic, and as fast as the CPU can walk the graph.
- **`LiveFeed`** waits out the gap each message originally arrived after and
  hands it over (`send`). Engine time becomes the wall clock.

Both deliver through a [`channel`](../../../src/channel.rs) source, which is the
one source that works in either mode — but that is an implementation detail of
the two impls. Everything downstream of `connect` — the book, both branches, the
join, the sink — is wired once and cannot tell which it got.

That is the seam a deployment actually swaps. A file replayed at its original
pace is a stand-in for a socket, not a socket; a real feed implements the same
trait, and nothing below it changes. This is the shape
[`run_mode`](../run_mode/) introduces, over real data.

### Output

Engine time on the left — replayed message time in a backtest, the wall clock
live. Timestamps are rebased to the first message, which really arrives at
09:30:00.

```text
0.021_311  bid  585.33  ask  585.91  spread  0.58  mid  585.62
0.197_502  bid  585.33  ask  585.92  spread  0.59  mid  585.62
0.197_540  bid  585.33  ask  585.93  spread  0.60  mid  585.63
0.201_332  bid  585.36  ask  585.93  spread  0.57  mid  585.64
0.267_498  bid  585.73  ask  585.74  spread  0.01  mid  585.74
0.270_775  bid  585.73  ask  585.75  spread  0.02  mid  585.74
...
46 quote changes
3.0s of market data replayed in 484.148µs — 6196× faster than real time
```

The last line is the backtest's headline: how much market time went through, and
what the wall clock took to do it. It is the reason a replay is worth having —
three seconds of book updates resolve in well under a millisecond, because
engine time is pure logic and no clock is waited on. Run with `-- realtime` and
the same three seconds take three seconds, by construction.

### Where to go next

- [`order_book`](../order_book/) — the same data through the CSV adapter, with
  fills and two-way prices written back out as files.
- [`odds_evens`](../odds_evens/) — the same split-and-recombine shape, reduced
  to its smallest form.
- [`run_mode`](../run_mode/) — swapping the *source* per run mode behind a
  trait, rather than per message inside one producer.
