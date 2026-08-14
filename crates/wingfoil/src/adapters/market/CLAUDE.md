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

## The five decisions the module docs pin

The `//!` header is the canonical statement; they are listed here so a change
to any of them is recognisable as a contract break, not a refactor.

1. **Fixed-point `Px`/`Qty`, parsed from the venue's decimal text.** The book
   keys levels by price, so a delete must match a key exactly; an `f64` round
   trip is how that silently stops matching. `Px::parse` is exact and rejects
   what it cannot represent (>9 dp, exponent notation, overflow) rather than
   truncating. `Px::try_from_f64` exists for tests and display, not for wire
   data, and is **fallible** — NaN, the infinities and out-of-range values are
   errors rather than a silent zero or a saturated bound.

   The backing store is `i128`, not `i64`. Price and quantity share one scale
   but want opposite things from it (price: precision at modest magnitude;
   quantity: magnitude at modest precision), and 9 dp in an `i64` caps both at
   ±9.22e9 — which makes SHIB/PEPE/BONK book levels *unrepresentable*, not
   merely imprecise. Do not narrow it back without re-checking that case.
2. **Two timestamps, different meanings.** `venue_time` is the venue's clock
   (optional, never trusted for cross-venue ordering); `recv_time` is engine
   time from `Ctx::time()`, which is what replay depends on. An adapter that
   stamps `recv_time` from `NanoTime::now()` has broken determinism — that is
   the single most likely adapter bug, and it will not show up in a live test.
3. **Gaps are signalled, never papered over.** `OrderBook::apply` clears the
   book, moves to `BookStatus::Gapped` and returns `BookApply::Gap(GapCause)`.
   The op still **ticks** on a gap: silence would leave downstream quoting off
   the last good value forever. `best_bid`/`best_ask`/`mid`/`depth`/
   `level_count` return empty while gapped, so a downstream that forgot to
   check `status()` still fails safe.

   Three distinctions here are load-bearing, and all three were flattened at
   some point in review:
   - `BookApply::Stale` (already covered by the image we hold — routine) vs
     `BookApply::Refused` (the book is gapped — broken until a snapshot). Same
     "we did not apply it", opposite obligations on the adapter.
   - `GapCause::Sequence { expected, got }` vs `GapCause::BufferOverflow
     { buffered }`. Overflow has no id pair; reporting a fabricated one says
     something false about what happened.
   - The cause is retained on the book (`OrderBook::gap_cause()`), because the
     op ticks the *book*, not the `BookApply`. Without that, the ids never
     reach the graph and a monitor cannot log which updates were lost.
4. **Pre-snapshot deltas are buffered, then replayed.** Every REST-snapshot +
   WS-delta venue has this race. `MAX_BUFFERED_DELTAS` caps the buffer and gaps
   out on overflow rather than growing without bound.

   The book only moves **forwards**: a snapshot a live book has already passed
   is `BookApply::Stale` and ignored, so a late or duplicate REST response
   cannot rewind the image while still reporting `Live`. The guard is scoped to
   live books — `gap_out` clears `last_seq`, so a recovery snapshot after a gap
   is always accepted whatever id it carries.
5. **A burst is applied in full, in order.** A book is a fold over its whole
   update history, so collapsing a burst latest-wins silently desynchronises
   it. Both `MarketEventOps` and `MarketBookOps` are implemented for the burst
   shape end to end so the group survives from source to book.

## Shapes

- **Every op is implemented for both the scalar and the `Burst` shape**, and
  the burst one is the shape a real adapter hits — `channel`/`external`/`spawn`
  sources all produce `Stream<Burst<T>>`. `MarketBookOps` covers
  `Stream<BookUpdate>` and `Stream<Burst<BookUpdate>>`; `MarketEventOps` uses
  associated types (`type Trades` / `type Books`) to demultiplex
  shape-preservingly, so `events.book_updates().order_book()` wires either way.

  `MarketEventOps` originally had only the scalar impl, which meant the wiring
  in the module docs did not compile against the stream a `channel` source
  hands you — and went unnoticed because that example was ```ignore```. It is a
  real doctest now. **If you add an op here, add both impls and make the doc
  example compile.**
- The demux ops are four hand-written ops rather than `map_filter` calls.
  `map_filter` demands a value in its false branch, so filtering a multiplexed
  venue stream through it constructed and discarded a `Trade::default()` — two
  `Arc<str>` allocations via `InstrumentId` — for every message that did not
  match, and it has no burst-preserving form at all.
- The book is held as `Arc<OrderBook>` and mutated through `Arc::make_mut`, so
  a cycle nobody retained mutates in place and a downstream that keeps one gets
  copy-on-write.
- `Cfg = ()`: the instrument comes from the first update rather than a config
  argument, and a second instrument on the same stream **aborts the run**. A
  mixed stream is a wiring bug (demultiplex with `MarketEventOps` first), not a
  runtime condition. The check runs per message, but via
  `InstrumentId::same_as`, which is two `Arc` pointer compares in the case that
  occurs (the adapter clones one id into every message) and falls back to
  content equality so independently-built ids still match. It is deliberately
  *not* a `debug_assert`: that would leave release builds silently interleaving
  two venues into one book.
- `InstrumentId` is built from `Sym`, the tree's shared interned-symbol type in
  `adapters/common.rs`. It used to be two bare `Arc<str>`s — a second, weaker
  copy of what `kdb.rs` already had. `kdb` now re-exports `Sym`/`SymbolInterner`
  from `common` for compatibility. Use `InstrumentId::interned` with a
  connection-lived `SymbolInterner` when building ids for many symbols.
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
  recover on its own. `BookApply::Refused` means a gap is still outstanding;
  `BookApply::Stale` is routine and needs no action. Do not collapse the two.
- Bursts are passed through intact. Never flatten a `Burst<MarketEvent>` to its
  last value on the way in.
- Instrument ids are built once per subscription (ideally through a
  `SymbolInterner`) and cloned into each message, not rebuilt per message.
- Tests replay recorded fixtures through `RunMode::HistoricalFrom(ZERO)` rather
  than hitting the live venue: hermetic CI, no API keys, no rate limits.

## Tests

Tier 1 only — there is no service to stand up.

- `src/adapters/market.rs` `mod tests` — fixed-point parse/display/ordering,
  the `i128` range, fallible `f64` conversion, `InstrumentId` identity and
  interning, and the book state machine (snapshot, delta, removal, gap, stale,
  snapshot regression, buffering, overflow, gap cause, derived prices).
- `tests/market_adapter.rs`, `#![cfg(feature = "market")]` — the op on a real
  graph: tick times, bursts, burst-preserving demux, the gap contract as
  downstream sees it, gap cause reaching downstream, the mixed-instrument
  abort, and replay determinism.
- The module `//!` example is a **real doctest**, not ```ignore``` — it is the
  only thing that keeps the documented wiring compiling.

## Not done yet

- **Python bindings.** `market` is absent from `wingfoil-python`'s feature
  list; adding them is a `/bind-adapter` job.
- **No example.** `examples/adapters/` has no `market/` directory, because a
  useful one needs a venue adapter to feed it.

```bash
cargo test -p wingfoil --features market
```
