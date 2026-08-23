# Market Adapter Example (wingfoil)

One strategy graph, two venues, both run modes. A single `FeedBuilder` trait
is the swap point between them:

- **`--historical`** — EUR/USD top-of-book quotes recorded in a **kdb+** table,
  replayed deterministically under `RunMode::HistoricalFrom`.
- **`--live`** — the same instrument from the **LMAX London demo**, over the
  FIX/TLS market-data session, under `RunMode::RealTime`.

Both implementations normalise into the [`market`](../../../src/adapters/market.rs)
adapter's venue-neutral vocabulary (`BookUpdate`, `Px`/`Qty`, `order_book()`),
so the strategy below the trait is written once and cannot drift between
backtest and live — the property the vocabulary exists to buy.

## The pattern

One trait, one implementation per run mode. The impl knows which clock it
needs (`fix_connect_tls` rejects a historical run at wiring; `kdb_read` needs
the historical window to slice its queries), and `main` never does:

```rust,ignore
trait FeedBuilder {
    /// The run mode this feed drives the graph under.
    fn run_mode(&self) -> RunMode;
    /// How long the run lasts.
    fn run_for(&self) -> RunFor;
    /// Wire the feed onto `g`, normalised to a stream of book updates.
    fn wire(&self, g: &GraphBuilder) -> Result<Stream<Burst<BookUpdate>>>;
}
```

```rust,ignore
let feed = feed_from_args()?;                  // KdbFeed or LmaxFeed

let g = GraphBuilder::new();
let books = feed.wire(&g)?.order_book();      // the shared strategy starts here
let _tops = books
    .map(|book: &Arc<OrderBook>| TopOfBook::of(book))
    .distinct()
    .with_time()
    .for_each(|(t, top)| {
        println!("{}  {top}", clock(*t));
        Ok(())
    });

g.build().run(feed.run_mode(), feed.run_for())?;
```

This is the swap point the
[trading roadmap](../../../../../docs/planning/trading-roadmap.md) describes:
live wires a venue, backtest wires a replay, `RunMode` decides — and the graph
between them is identical.

The two impls also show the two honest ways into fixed-point prices:

- **Live** parses the venue's own decimal text — `Px::parse`, no `f64` on the
  parse path (decision 1 of the vocabulary).
- **Historical** stores integer pipettes (10⁻⁵) in kdb+ and scales them with
  `Px::from_raw` — exact integers end to end, so the replayed book carries the
  recorded prices bit-for-bit.

## Run: historical (kdb+ replay)

Start kdb+ on port 5000 with the bundled synthetic history — a deterministic
one-minute random walk (see [`data/quotes.q`](data/quotes.q)):

```sh
q crates/wingfoil/examples/adapters/market/data/quotes.q -p 5000
```

Then, in another terminal:

```sh
cargo run -p wingfoil --features market,fix,kdb --example market_adapter -- --historical
```

`KDB_HOST` / `KDB_PORT` override `localhost:5000`.

## Output

40 lines, one per quote that moved the top, at the *recorded* timestamps —
the graph clock is driven by the data, and the data is a fixed walk, so a
re-run prints exactly these quote lines (trimmed here; the elapsed figure on
the final line is wall-clock and varies run to run):

```text
00:00:00.000  bid 100000 @ 1.0931 | ask 300000 @ 1.09312 | mid 1.093110
00:00:01.500  bid 200000 @ 1.0931 | ask 200000 @ 1.09314 | mid 1.093120
00:00:03.000  bid 300000 @ 1.0931 | ask 100000 @ 1.09316 | mid 1.093130
00:00:04.500  bid 400000 @ 1.09312 | ask 300000 @ 1.09314 | mid 1.093130
00:00:06.000  bid 500000 @ 1.0931 | ask 200000 @ 1.09314 | mid 1.093120
...
00:00:57.000  bid 400000 @ 1.09311 | ask 100000 @ 1.09317 | mid 1.093140
00:00:58.500  bid 500000 @ 1.09312 | ask 300000 @ 1.09314 | mid 1.093130
40 book updates in 3.829ms
```

The 70-second window is read as three 30-second slices (one query each); run
with `RUST_LOG=info` to watch them.

## Run: live (LMAX London demo)

1. Register a free demo account at
   <https://register.london-demo.lmax.com/registration/LMB/>.
2. Export the credentials and switch the flag:

```sh
LMAX_USERNAME=you LMAX_PASSWORD=secret \
  cargo run -p wingfoil --features market,fix,kdb --example market_adapter -- --live
```

The graph is the same; only the feed impl changed. `fix_connect_tls` connects
and logs on at graph `start()`, `fix_sub` sends the EUR/USD MarketDataRequest
once the session reports `LoggedIn` (watch it with `RUST_LOG=info`), and each
`MarketDataSnapshotFullRefresh` (35=W) prints as a top-of-book line in the
same format as the replay — for 60 seconds, then the run ends.

## Code

The live decode walks the FIX repeating group with
[`FixMessage::groups`](../../../src/adapters/fix.rs) — `field(270)` alone would
see only the first entry's price:

```rust,ignore
for entry in msg.groups(268, 269) {          // NoMDEntries / MDEntryType
    let side = match entry.field(269) {
        Some("0") => Side::Bid,
        Some("1") => Side::Ask,
        _ => continue,
    };
    let level = Level::new(
        Px::parse(entry.field(270).context("no MDEntryPx")?)?,
        Qty::parse(entry.field(271).context("no MDEntrySize")?)?,
    );
    // …
}
```

The historical impl reads typed rows through `KdbDeserialize` and stamps
`recv_time` from the tick time — which under replay *is* the recorded
timestamp, exactly what decision 2 of the vocabulary asks for.

## See also

- [`../../core/order_book/`](../../core/order_book/) — a matching-engine order
  book maintained *from* raw orders; this example maintains a levels book from
  venue images.
- [`../fix/`](../fix/) — the FIX session machinery on its own, self-contained.
- [`../kdb/`](../kdb/) — the time-sliced read model on its own.
- [`../../core/run_mode/`](../../core/run_mode/) — the two clocks that make
  the swap safe.
