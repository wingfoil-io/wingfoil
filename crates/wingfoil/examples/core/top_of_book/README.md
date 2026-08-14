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
let (inbound, sender) = g.channel::<Message>();

// The apex: one node maintains the book.
let top = inbound.map(move |burst| apply(burst, &book));

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

### Why a `channel` source

`channel` is the one source that works in **both** run modes:

- **Historical** — the producer stamps each message with its own time
  (`send_at`) and the engine replays it on the graph clock, consulting no
  wall clock at all. Deterministic, and as fast as the CPU can walk the graph.
- **Realtime** — the producer waits out the gap each message originally arrived
  after and hands it over (`send`). Engine time becomes the wall clock.

The producer is the only part of the program that knows which mode it is in.
Everything downstream of the channel — the book, both branches, the join, the
sink — is wired once and cannot tell the difference.

A file replayed at its original pace is of course a stand-in for a live socket,
not a live socket. What is *not* a stand-in is the graph: swap the producer for
a real feed and nothing below it changes.

### Output

Engine time on the left — replayed message time in a backtest, the wall clock
live. Timestamps are rebased to the first message, which really arrives at
09:30:00.

```text
0.000_000  bid  585.09  ask  585.34  spread  0.25  mid  585.21
...
```

### Where to go next

- [`order_book`](../order_book/) — the same data through the CSV adapter, with
  fills and two-way prices written back out as files.
- [`odds_evens`](../odds_evens/) — the same split-and-recombine shape, reduced
  to its smallest form.
- [`run_mode`](../run_mode/) — swapping the *source* per run mode behind a
  trait, rather than per message inside one producer.
