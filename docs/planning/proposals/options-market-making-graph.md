# Options market making on a live feed — the graph shape

**Status: designed, not built; no tracking issue yet.** This is a design body
for wiring a *specific* application on top of the engine: streaming options
market data off a WebSocket venue, calibrating a volatility smile per expiry,
quoting per instrument, and hedging an aggregate delta. It sits above
[`trading-stack.md`](trading-stack.md) (**Project Venue**) — that page designs
the execution vocabulary this one consumes, and §9 below is the list of pieces
this design needs from it.

Nothing here requires an engine change. Every question it answers is a wiring
question, and the answers are mostly rulings about *where state lives* rather
than about which combinator to call.

## 1. The problem

Three views of the same feed, at three different granularities, and they are
not independent:

- **Per expiry** — the strikes of one expiry, plus spot and the future, are
  what a smile is calibrated from. Tens of instruments, one output.
- **Per instrument** — the calibrated smile produces a theo per strike, which
  drives a quote, an order state machine and a fill stream. One input, many
  outputs.
- **Portfolio** — every position across every expiry, aggregated into a delta
  (and vega, and whatever else) that drives a hedge. Many inputs, one output.

And the instrument set is not static: new expiries list, new strikes appear
within a live expiry, old ones delist, all while the graph is running.

## 2. The shape

It is **one graph**, not three. Fan out by key, fan in to calibrate, fan back
out to quote, fan in again to hedge:

```
   ws frames (one stream)  ──►  decode  ──►  Burst<MarketEvent>
          ▲                                       │
          │ subscribe / unsubscribe                ├── spot, future ────┐
          │                                        │                    │
   instrument feed ──► add / del keys              └── demux by EXPIRY ─┤
                                                                        ▼
                                        per-expiry op: BTreeMap<Strike, Quote>
                                        absorb the burst ─► calibrate ─► SmileParams
                                                                        │
                        ┌───────────────────────────────────────────────┤
                        ▼                                               ▼
          demux by INSTRUMENT: theo, edge, quote,          fold positions ─► greeks
          order state machine ──────────► orders           ─► portfolio delta ─► hedge
                        ▲                                               ▲
                        └───────────── fills ◄── venue ─────────────────┘
                                       (feedback, time + 1)
```

## 3. The one decision everything else follows from

**Instrument → expiry → instrument is not a cycle, and must not be wired as
one.**

It looks circular on the diagram, and the instinct is to reach for `feedback`.
Don't. Calibration consumes the book at time *t*; quoting consumes the
calibration at time *t*. That is a pure fan-in followed by a fan-out, resolved
by topological order **within a single cycle**. Inserting a `feedback` edge
there would cost a cycle of staleness for nothing, and would quietly make every
quote one instant behind the book it was derived from.

The only genuine cycle is **order → fill → position → quote**, and that one
takes `feedback`, whose `time + 1` semantics are exactly right:
[`trading-stack.md`](trading-stack.md) §5.1 makes the argument — you cannot
legitimately quote on a fill in the same instant you sent the order, so the
engine's causality rule and the domain's agree.

Corollary: anything the quote needs *within* the cycle (smile params, a risk
snapshot, a position limit) must arrive down the fan-out, not around the
feedback edge.

## 4. Keying: demux, not graph dynamism

Both mechanisms are available and the
[`examples/core/dynamism/`](../../../crates/wingfoil/examples/core/dynamism/)
group demonstrates all four. For this application, **`demux_it` on a fixed slot
pool** is the right default at both levels.

| | `demux_it` | `dynamic_group` |
|---|---|---|
| Topology | fixed; slots recycled on `DemuxEvent::Close` | spliced at runtime |
| Feature gate | none | `dynamic-graph` |
| Cost when membership churns | zero | scheduler cycles per add/delete |
| `RunFor::Cycles` | means what it says | no longer maps onto ticker ticks |
| Cost you accept | choose a capacity, wire the overflow child | splicing latency mid-session |

The deciding facts for an options chain: every strike gets the *same* pipeline
(so there is nothing structural for a factory to vary), and concurrent
instrument count is boundable even though all-time count is not. Slot recycling
tracks the former, which is the quantity that matters.

`demux_it` rather than `demux_map` because a single WebSocket burst legitimately
carries updates for several instruments — and a delist alongside a price — and
`demux_map` routes exactly one value per cycle.

**Wire the overflow child.** `demux_it` returns it as the second element of the
tuple and an unwired one swallows events silently. Abort the run or alert; do
not let a capacity miscalculation become silent data loss. Both dynamism
examples in the tree abort, and that is the pattern to copy.

Reach for `dynamic_group` only if concurrent instrument count turns out to be
genuinely unbounded.

## 5. Identity: which key

`market.rs` already has `InstrumentId { venue: Sym, symbol: Sym }`, and `Sym` is
an `Arc<str>` behind a `SymbolInterner`. Interning buys two things — one
allocation per distinct symbol, and a `ptr_eq` fast path for *equality*
(`InstrumentId::same_as`). It does **not** make hashing cheaper: `Sym` derives
`Hash` on the `Arc<str>`, so hashing walks the string contents like any other.

That matters because `demux_it`'s routing is a hash lookup. Per routed item, in
steady state, `DemuxMap::get_or_insert` costs one `HashMap::get` plus a
`RefCell::borrow_mut`, and no allocation — the `BTreeSet` of free slots is
touched only on add and close. So the key type is the whole cost.

**The ruling: key on the venue's own integer id where the venue publishes a
stable one** (FIX `SecurityID` and most exchange feeds do), and otherwise on an
interned integer. Never on the symbol string.

The argument is not primarily performance. It is **reproducibility**: an id
assigned by arrival order gives instrument 7 a different number when the same
recording is replayed from a different start point, which breaks log
comparability and makes a backtest's slot assignment depend on where it
started. A venue id is stable forever and free. Where a venue is symbol-only —
most crypto venues are; Deribit's identity is `instrument_name` — seed the
interner from the instrument snapshot in a **deterministic order** (sort by
symbol) rather than by arrival, and reproducibility is recovered.

Either way, the symbol string lives in exactly one side table, consulted at
egress and logging only.

### 5.1 Dense slots: the optimisation to *not* do first

There is a further step — resolve each instrument to a dense `u32` at decode,
carry only that in the message, and route with the raw `Builder::demux`
(`Fn(&T) -> usize`), which has no map at all and costs an array index. It also
turns every downstream per-instrument collection into a `Vec` indexed by slot
rather than a hash map, which is the larger prize: the portfolio fold becomes a
linear scan over contiguous memory.

**This is recorded here so it is not re-derived, and explicitly deferred.** The
saving is on the order of the `NanoTime::now()` read the benches measure at
~24 ns, against a WebSocket path whose jitter is milliseconds, and it trades
`demux_it`'s correct-by-construction lifecycle (slot reuse, reset on close,
overflow) for hand-rolled table management. Calibration will dominate routing
by orders of magnitude.

Start on `demux_it` with an integer key. Intern to dense slots only if a profile
says routing is hot — a contained change, because the key type is the only thing
that moves.

**The trap, if it is ever done:** a dense slot is *identity*, assigned
arbitrarily and stable for the instrument's lifetime. It is not strike rank.
Making the slot index track strike order means every new listing shifts every
existing slot and invalidates every position array. Strike order is a separate
sorted view (§6), and a new listing costs one insert into it while the slots
stay untouched.

## 6. Per-expiry state lives in one op, not in N streams

The reduction is the part most likely to be wired wrong, and it is worth being
blunt about:

**A burst carrying 40 strike updates for one expiry should activate one node.**
That node holds `BTreeMap<Strike, Quote>` as its `State`, absorbs the whole
burst, and calibrates once. Wiring a stream per strike and `join`ing them into a
calibrator pays N node activations to produce one output, and needs rewiring
every time a strike lists.

So: **per-instrument nodes exist only where per-instrument state lives** — the
order state machine, the position, the working-order set. They are not a data
reduction mechanism. The expiry-level demux exists to partition the burst; the
instrument-level demux exists to hold order state.

The same rule governs the portfolio view. Positions are a `fold` over the single
fill stream into a keyed collection, joined against the per-expiry
`SmileParams` (few keys, cheap). Not a fan-in of N position streams.

## 7. Rates: throttle the derived edges, never the feed

Ingest every tick. Recalibrate and re-hedge on a clock:

- `throttle(..)` or `sample(ticker)` on the calibrate edge, and again on the
  hedge edge.
- Hedging off every market-data tick is both expensive and wrong — it converts
  spread into fees.
- Under `RunMode::HistoricalFrom` these are engine-time decisions, so a backtest
  recalibrates on exactly the same instants as it would live. That is the whole
  point of the two-clock split, and it is why the throttle must key off
  `Ctx::time()`, never `wall_time()`.

## 8. Backtest parity

The ingress seam is what makes the same graph replayable:

- Recorded WebSocket frames replay through a `channel` source with `send_at`,
  which is timestamped and deterministic in historical mode.
- **Subscription control is a side channel, not graph structure.** The venue's
  instrument feed drives an `add`/`del` key stream; that stream drives two
  things *independently* — a sink that writes subscribe/unsubscribe frames, and
  the demux key lifecycle inside the graph. In backtest the sink is a no-op
  while the keys still need to exist. Coupling them means the graph cannot
  replay, and it is the easiest mistake here to make.
- The subscribe ack arrives later than the decision that caused it, which is a
  second reason those two must not be the same edge.
- The execution seam is Project Venue's: live wires a venue sink, backtest wires
  `SimVenue`, `RunMode` decides.

## 9. What this needs that does not exist

From the tree today: the `ws` adapter, `market::{Px, Qty, OrderBook,
MarketEvent}`, `Burst`, `demux_it`, `feedback`, `channel` replay, latency
stamping. All present.

Absent, and all of it named in [`trading-stack.md`](trading-stack.md) §1: an
`Order` type, a `Fill` type, order state, position, pre-trade risk, and
`SimVenue`. The per-instrument pipeline in §2 **is** that layer, so this design
does not sit on top of Project Venue so much as motivate it — and it is a
concrete consumer to design §4's fill model against.

Nothing options-specific (a pricer, a smile parameterisation, a calibrator)
belongs in this tree. Those are application code above the engine.

## 10. Open questions

1. **Worst-case concurrent instrument count.** Decides demux capacity, and
   whether §4's ruling holds at all.
2. **One socket or many.** If the venue shards subscriptions across
   connections, ingress becomes N threaded sources merged, and subscription
   management grows a connection-lifecycle dimension §8 does not cover.
3. **Calibrate from quotes or from trades/mid.** Decides whether the per-expiry
   op needs full book state or only top-of-book — which is the difference
   between holding N `OrderBook`s and holding a `BTreeMap<Strike, (Px, Px)>`.
4. **Single process or split.** `examples/showcase/trading_e2e` has the
   multi-process pattern with latency stamping across hops, if md-handler,
   strategy and gateway want separating.
5. **Does the quote need position skew.** If yes, a risk snapshot must ride the
   §3 fan-out rather than the feedback edge, and the portfolio fold moves
   upstream of the instrument demux.
