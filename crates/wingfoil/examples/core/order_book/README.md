## Order book: one stateful node, two output streams

A stateful streaming example: a limit order book maintained entirely in `fold`
state. Each tick applies one synthetic message — a resting limit order, or an
occasional aggressive order that crosses and consumes a level — and the graph
emits **two** streams from that one node: the *fills* it produced, and the *top
of book* (best bid and best ask).

The two tick at different rates, which is the point. Every message moves the
prices; only some produce a fill.

```rust
// Fold state is `(Book, Option<Fill>)`: the book carries across ticks, the
// fill is what *this* message produced.
let applied = g
    .ticker(Duration::from_millis(1))
    .fold((Book::new(seed), None), |state, _| {
        state.1 = state.0.apply();
    });

// One node, two outputs at different rates.
let (fills, tops) = applied
    .map(|(book, fill)| (fill.clone(), Some(book.top())))
    .split_some();

let _prices = tops.distinct().for_each(|p| { println!(...); Ok(()) });
let _fills = fills.for_each(|f| { println!(...); Ok(()) });
```

Sample output:

```text
price   bid  99   ask   -   spread -
price   bid  99   ask 103   spread 4
fill    buy    7 @ 103
price   bid  99   ask   -   spread -
price   bid  99   ask 104   spread 5
price   bid  99   ask 101   spread 2
price   bid 100   ask 101   spread 1
fill    buy    1 @ 101
price   bid 100   ask 102   spread 2
fill    sell  15 @ 100
```

### Scope and idiom

The load-bearing idea is an arbitrary stateful aggregation (a `BTreeMap`-backed
order book) carried in `fold` state, fanned out into two independently-ticking
streams, and drained through `for_each` sinks — all in the fluent API.

**How two outputs come out of one node.** An `Op` has a single `Out` by
construction: that is what lets the engine own every value slot, and what makes
the compiled tiers possible. So a node that wants to emit two things emits *one*
value describing both — here `(Option<Fill>, Option<TwoWayPrice>)` — and
[`split_some`](https://docs.rs/wingfoil/latest/wingfoil/fluent/struct.Stream.html)
fans it out at wiring time, giving each branch its own tick. Use plain `split()`
when both outputs really do tick together.

Both branches read the same upstream node, so the book update happens once
however many branches read it.

This remains a *simplified* port of the legacy `order_book` example in one
respect: the legacy version reads real LOBSTER market-data CSV through a
`lobster` matching engine and writes its two streams to separate CSV files. The
wingfoil engine has a `csv` feature (`examples/csv_adapter`), but this example
keeps its feed self-contained — a deterministic LCG — so it runs with no data
file and no extra feature flag.
