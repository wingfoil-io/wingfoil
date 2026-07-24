## Order book: stateful top-of-book

A stateful streaming example: a limit order book maintained entirely in `fold`
state. Each tick applies one synthetic message (a resting limit order, or an
occasional aggressive order that crosses and consumes a level), and the graph
emits the *top of book* — best bid and best ask — but only when it changes.

```rust
let book = g
    .ticker(Duration::from_millis(1))
    .fold(Book::new(seed), |book, _| book.apply());

let top = book.map(|b| b.top()).distinct();
let _out = top.for_each(|p| { println!(...); Ok(()) });
```

Sample output:

```text
bid  99   ask   -   spread -
bid  99   ask 101   spread 2
bid 100   ask 101   spread 1
...
```

### Scope and idiom

This is a *simplified* port of the classic `order_book` example. The classic
version reads real LOBSTER market-data CSV, executes it through a `lobster`
matching engine, and `split()`s the result into two output streams — a fills
stream and a prices stream — written to separate CSV files.

Two of those pieces are **not yet available on the next engine**, so the port
takes a different shape rather than forcing them:

- **`split()` / multi-output nodes** — the classic example emits *both* fills
  and prices from one node. The next engine's ops are single-output, so this
  port emits a single top-of-book stream. (A stateful book that produced fills
  and prices would need a demux/split op.)
- **The CSV replay + write adapter** — the next engine has a `csv` feature
  (`examples/csv_adapter`), but this example keeps its feed self-contained (a
  deterministic LCG) so it runs with no data file and no extra feature flag.

What it *does* demonstrate, cleanly, is the load-bearing idea: an arbitrary
stateful aggregation (`BTreeMap`-backed order book) carried in `fold` state,
projected with `map`, de-duplicated with `distinct`, and drained through a
`for_each` sink — all in the fluent API.
