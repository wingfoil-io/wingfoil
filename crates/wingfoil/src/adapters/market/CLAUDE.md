# market adapter (wingfoil)

`src/adapters/market.rs`, feature `market`. **No legacy twin** — this is
wingfoil-only, like `lines`.

## What it is, and what it deliberately is not

It is the **venue-neutral vocabulary** that market data adapters normalise
into, plus the order book state machine that consumes it. It connects to
nothing and has no external dependencies: transform ops only, the same shape as
`augurs`.

It is **not** a venue adapter, and no venue adapter belongs in this tree.
Binance, Coinbase, Databento and friends live in their own crates, depending on
`wingfoil = { features = ["market"] }` and adding their own extension traits
through the public `GraphBuilder::source` / `Stream::wire` seams. That split is
load-bearing in two directions: their transport dependencies (websockets, TLS,
venue SDKs) never enter this crate's dependency graph or CI matrix, and this
crate stays neutral infrastructure.

## The four decisions the module docs pin

The `//!` header is the canonical statement; they are listed here so a change
to any of them is recognisable as a contract break, not a refactor.

1. **Fixed-point `Px`/`Qty`, parsed from the venue's decimal text.** The book
   keys levels by price, so a delete must match a key exactly; an `f64` round
   trip is how that silently stops matching. `Px::parse` is exact and rejects
   what it cannot represent (>9 dp, exponent notation, overflow) rather than
   truncating. `Px::from_f64` exists for tests and display, not for wire data.
2. **Two timestamps, different meanings.** `venue_time` is the venue's clock
   (optional, never trusted for cross-venue ordering); `recv_time` is engine
   time from `Ctx::time()`, which is what replay depends on. An adapter that
   stamps `recv_time` from `NanoTime::now()` has broken determinism — that is
   the single most likely adapter bug, and it will not show up in a live test.
3. **Gaps are signalled, never papered over.** `OrderBook::apply` clears the
   book, moves to `BookStatus::Gapped` and returns `BookApply::Gap`. The op
   still **ticks** on a gap: silence would leave downstream quoting off the last
   good value forever. `best_bid`/`best_ask`/`mid`/`depth` return empty while
   gapped, so a downstream that forgot to check `status()` still fails safe.
4. **Pre-snapshot deltas are buffered, then replayed.** Every REST-snapshot +
   WS-delta venue has this race. `MAX_BUFFERED_DELTAS` caps the buffer and gaps
   out on overflow rather than growing without bound.

## Shapes

- `order_book` is implemented twice — `OrderBookOp` on `Stream<BookUpdate>` and
  `OrderBookBurstOp` on `Stream<Burst<BookUpdate>>` — behind one
  `MarketBookOps` trait. The burst impl is the one a real adapter hits, since
  `channel`/`external` sources produce bursts, and it **applies every update in
  the group in order**. Collapsing a burst latest-wins would drop intervening
  level changes and desynchronise the book; `burst_applies_every_update_not_
  just_the_last` in `tests/market_adapter.rs` pins this.
- The book is held as `Arc<OrderBook>` and mutated through `Arc::make_mut`, so
  a cycle nobody retained mutates in place and a downstream that keeps one gets
  copy-on-write.
- `Cfg = ()`: the instrument comes from the first update rather than a config
  argument, and a second instrument on the same stream **aborts the run**. A
  mixed stream is a wiring bug (demultiplex with `MarketEventOps` first), not a
  runtime condition.
- Every event type derives `Default` only because the engine requires it for
  the pre-first-tick value slot. `BookUpdate`/`MarketEvent` need hand-written
  impls (derive `Default` on an enum needs a unit variant). None of these
  defaults are meaningful values.

## What an out-of-tree venue adapter owes

If you are reviewing or writing one, these are the checks:

- Prices and quantities go through `Px::parse` / `Qty::parse` on the venue's
  own text. No `f64` in the parse path.
- `recv_time` comes from `Ctx::time()`.
- The venue's sequencing maps onto `Sequencing::Single` or `Sequencing::Span`;
  `Sequencing::None` only when the venue genuinely sends no sequence number,
  since it forfeits gap detection entirely.
- `BookApply::Gap` is acted on — re-request a snapshot. The book will not
  recover on its own.
- Tests replay recorded fixtures through `RunMode::HistoricalFrom(ZERO)` rather
  than hitting the live venue: hermetic CI, no API keys, no rate limits.

## Tests

Tier 1 only — there is no service to stand up.

- `src/adapters/market.rs` `mod tests` — fixed-point parse/display/ordering and
  the book state machine (snapshot, delta, removal, gap, stale, buffering,
  overflow, derived prices).
- `tests/market_adapter.rs`, `#![cfg(feature = "market")]` — the op on a real
  graph: tick times, bursts, the gap contract as downstream sees it, the
  mixed-instrument abort, and replay determinism.

## Not done yet

- **Python bindings.** `market` is absent from `wingfoil-python`'s feature
  list; adding them is a `/bind-adapter` job.
- **No example.** `examples/adapters/` has no `market/` directory, because a
  useful one needs a venue adapter to feed it.

```bash
cargo test --manifest-path crates/wingfoil/Cargo.toml --features market
```
