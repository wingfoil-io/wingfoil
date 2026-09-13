# Project Venue — the execution layer, the design

**Status: designed, not scheduled; tracking issue to be filed.** This is the
design body for the trading layer named in
[`../trading-roadmap.md`](../trading-roadmap.md) §3 and item 6 of §4 — the
build-out *up* the stack, as
[`kernel-bypass-io.md`](kernel-bypass-io.md) (**Project Bypass**) is the
build-out *down* it. Per [`../../README.md`](../../README.md), the tracking
issue is the status and this page carries the reasoning — so until that issue
exists, §9's gates are a proposal and not a schedule.

The claim under examination is one sentence from the roadmap: **"the simulated
venue is just an op."** If it holds, wingfoil gets end-to-end strategy
backtesting for a fraction of what a platform costs, because the property
incumbents sell — identical code backtest and live — is already the property
the engine enforces. If it does not hold, everything above it is built on
sand. Most of what follows is about finding that out cheaply.

## 1. What already exists — an audit

The layer is not being started from nothing, and being precise about the
starting point changes what phase 0 is.

| Piece | Where | State |
|---|---|---|
| Fixed-point price/quantity | `adapters/market.rs` — `Px`, `Qty` (`fixed_point!`, `DECIMALS = 9`) | Complete. `Ord + Eq + Hash`, parsed from venue decimal text, never through `f64` |
| Instrument identity | `market.rs` — `InstrumentId` over `Sym`/`SymbolInterner` | Complete, with interning |
| Side, levels, trades, book events | `market.rs` — `Side`, `Level`, `LevelChange`, `Trade`, `BookSnapshot`, `BookDelta`, `BookUpdate`, `MarketEvent` | Complete |
| Book maintenance | `market.rs` — `OrderBook` with gap detection, pre-snapshot delta buffering, stale-snapshot protection; `best_bid`/`best_ask`/`depth`/`mid`/`spread`/`microprice` | Complete, and it is the matching surface a sim needs |
| Burst discipline | `runtime/burst.rs` — `Burst<T> = TinyVec<[T; 1]>`; `MarketEventOps`/`MarketBookOps` implemented for scalar *and* burst shapes | Complete, and the convention to follow |
| FIX session | `adapters/fix.rs` — initiator (`fix_connect*`), acceptor (`fix_accept*`), TLS, sequence validation, resend/GapFill, `AlwaysSpin` | Complete at the session level. Application messages are opaque `FixMessage` |
| Feedback edges | `fluent.rs` / `interp.rs` — `feedback` + `feedback_send` | Complete, and §5 shows it already answers the loop's hardest question |
| Latency stamping | `latency`, `Traced<T, L>` | Complete, and free to extend across the execution hop |
| An order→fill loop, end to end | `examples/showcase/trading_e2e` | Exists as a *demo*: `OrderFrame`/`FillFrame` in `shared.rs`, `u8` side, qty as `u64`, price in bps, `client_seq` standing in for order identity, no order state |

What does **not** exist anywhere in the tree: an `Order` type, a `Fill` type,
order state, position, or any mapping between the trading types and FIX
application messages. `fix_gw.rs` builds a NewOrderSingle by hand from raw tag
numbers (`59` for TimeInForce, `2` for OrdType) — which is the evidence that
the mapping is real work that every consumer would otherwise redo.

So the market-data half of the vocabulary is done and good. The execution half
is absent. That is a much smaller starting gap than "build a trading platform"
suggests, and it is the reason this project is worth scoping tightly.

## 2. The one decision everything else follows from

**The swap point is the typed order stream, above FIX.**

The strategy emits orders and consumes fills. Live wires a venue sink and a
fill source; backtest wires a `SimVenue` op against the replayed book;
`RunMode` decides which. FIX never learns that a simulator exists, and the
simulator never encodes a tag.

```text
                      ┌──────────────────────────────┐
  market data ───────▶│          strategy            │◀─── fills
                      └──────────────┬───────────────┘
                                     │ orders
                    ┌────────────────┴────────────────┐
                    │                                 │
              RunMode::RealTime              RunMode::HistoricalFrom
                    │                                 │
            ┌───────▼────────┐               ┌────────▼────────┐
            │  order codec   │               │    SimVenue     │
            │  ↕ FIX / venue │               │  (+ book, fees) │
            └───────┬────────┘               └────────┬────────┘
                    │ fills                           │ fills
                    └────────────────┬────────────────┘
                                     │
                              position / PnL fold
```

Everything else in the roadmap's §3 list is downstream of this being right:
position keeping is a fold over fills, risk is a filter on the order stream,
PnL is a join of position and mid. None of those is hard. **The swap point is
the only architectural claim, so it is the only thing phase 0 should try to
prove.**

The alternative — swapping at the FIX byte level, with a simulated acceptor
answering NewOrderSingle — is rejected for backtesting in §6.2. It is a real
tool, for a different job.

## 3. The vocabulary, and where it lives

### 3.1 The `market.rs` precedent settles the in-tree question

`market.rs` states the rule already, for market data:

> Venue adapters are **separate crates** rather than modules here: each
> carries its own transport dependencies and release cadence, and wiring one
> in costs this crate nothing. What lives here is the part they must all agree
> on.

The execution layer takes the same split, for the same reason. **The
vocabulary is in tree; the machinery is out of it.** In tree, feature-gated
and out of the prelude exactly as `market` and `statistics` are:

- `Order`, `Fill`, and the enums they need.
- The FIX codec between them and application messages (§6.1).

Out of tree, as separate crates: `SimVenue`, the OMS and order state machine,
risk, portfolio and venue-specific execution adapters.

The dependency direction is what forces it. `adapters/fix` and
`adapters/market` are in tree; the moment either speaks the order vocabulary —
and §6.1 says `fix` must — the type has to be in tree too, or you get a crate
that depends on `wingfoil` while `wingfoil` depends back on it. This is the
`wingfoil-wire-types` situation with the same answer.

The counter-position, considered and rejected: keep the adapters ignorant, let
FIX stay field-level, and put the mapping entirely out of tree. It keeps the
line cleaner, but it means every consumer rewrites the ExecutionReport → `Fill`
mapping — precisely the duplication the `market` layer exists to prevent.

### 3.2 The types

`Order` is designed **from the FIX side**, not the simulator side. The sim can
consume anything; FIX cannot express anything. Design it against what a
NewOrderSingle and an ExecutionReport actually need and the sim's requirements
fall out as a subset; do it the other way and the codec fights the type
forever.

That means, at minimum: a client order id, `InstrumentId`, `Side`, quantity as
`Qty`, limit price as `Option<Px>`, an order type, a time-in-force, and — on
the fill side — an execution id, filled and remaining `Qty`, fill `Px`, fees,
and the venue/receive timestamp pair.

Two things carry straight over from `market.rs` and are not re-litigated:
prices and quantities are `Px`/`Qty`, never `f64`; and both timestamps are
recorded, with `recv_time` set from `Ctx::time` so a recorded session stays
replayable.

### 3.3 The shape is `Burst`, on both edges

Not an incidental detail. `Burst<T>` is `TinyVec<[T; 1]>`, so the scalar case
costs nothing, and `market.rs` already implements its ops for both shapes via
associated-type traits (`MarketEventOps { type Trades; type Books; }`) with
paired op structs. `SimVenue` follows that convention rather than picking one
shape.

But there is a stronger reason here than ingress coalescing: **fills are
intrinsically burst-shaped.** One order crossing several book levels produces
several fills at the same instant. Even a single scalar `Order` in yields
`Burst<Fill>` out. A 1:1 `Order → Fill` signature would be wrong from the
first line, and would have to be unpicked later at the worst possible moment —
after strategies depend on it.

The sim's inputs are burst on both edges too: `Burst<BookUpdate>` from the
replayed feed and `Burst<Order>` from the strategy, arriving in the same
cycle. §4.2 is about the question that raises.

### 3.4 Identity

Orders carry an id because FIX requires ClOrdID and because an OMS is
meaningless without one. It also interacts with `TimeQueue` dedup — see §5.2,
where the burst shape turns out to defuse most of the risk.

## 4. The simulated venue

### 4.1 Phase 0 builds a deliberately dishonest one

Fill at touch, immediately, no queue model, no fees, no modelled latency. It
is known to lie — the roadmap says so plainly ("a naive fill-at-touch
simulator lies") — and that is the point. ("No modelled latency" rather than
"no latency": §5.1's one-nanosecond `feedback` floor still applies, because it
is structural and cannot be opted out of. P0 adds nothing on top of it.)
**The deliverable of phase 0 is not fill quality, it
is the closed loop:** strategy → order → fill → position → strategy, running
under `RunMode::HistoricalFrom(NanoTime::ZERO)` with exact values *and* tick
times asserted, per the repo's test conventions.

The reasoning is a claim about where the risk sits. The loop either works or
it doesn't, and which it is depends on cycle semantics, not on fill realism —
so a dumb simulator surfaces it in days. A queue-position model surfaces it in
a quarter, and if the answer is bad, the quarter is gone. Fill quality is an
unbounded axis of modelling opinions; it must not be on the critical path to
finding out whether the architecture holds.

### 4.2 The decisions a fill model must make explicitly

By analogy with `market.rs`'s "five decisions an adapter must not make for
itself", these are the ones a sim venue must state rather than let fall out of
wiring order. Each has real PnL consequences and each is invisible in a
backtest that gets it wrong.

**1. Order-vs-update ordering within a cycle.** An order emitted at *t*
reaches the sim at *t+1* (§5.1), where it arrives in the same cycle as *that*
instant's `Burst<BookUpdate>`. Does it match against the book before or after
those updates apply? Matching *after* lets a strategy react to a book update
and fill against it at the same instant — lookahead, dressed as speed. The
conservative answer is that an order sees the book as of the previous instant,
and this should be a stated property, not an emergent one.

**§5.1's `+1` does not settle this, and the two are easy to conflate.** The
feedback floor guarantees the *strategy* cannot observe a fill at the instant
it ordered; it says nothing about which side of the sim's own cycle the book
update lands on. The question simply recurs one cycle later, inside the sim,
where the engine offers no structural answer — so this is the sim's decision to
make and to document. Pin it in P0's test alongside the carried burst shape:
the assertion is cheap to write while the loop is four ops long, and expensive
to retrofit once a fill model sits on top of it.

**2. Latency is modelled in engine time.** Order → ack → fill takes time at a
real venue. In a backtest that delay must be scheduled in engine time
(`Ctx::time`), never derived from the wall snap, or the determinism the whole
project rests on evaporates at exactly the boundary being simulated. §5.1
shows the engine already imposes a one-nanosecond floor for free; a realistic
model schedules a larger, configurable delay on the same mechanism.

**3. Queue position is a model, and its assumptions are stated.** A resting
limit order at a price level is behind some unknown quantity. Assuming the
back of the queue is pessimistic; assuming the front is a fantasy that makes
every passive strategy look profitable. P2 picks a conservative default and
names it in the docs.

**4. Partial fills are the normal case.** See §3.3 — the output is a burst,
and remaining quantity is order state. P0 can keep that state inside the sim,
which is the least committal place for it; who owns it once an OMS exists is
open (§12), and this decision does not pre-empt that.

**5. Fees are not optional.** A maker/taker schedule flips the sign of most
passive strategies. A sim without a fee model is not conservative, it is
wrong in a specific and flattering direction.

### 4.3 One sim, many venues — and what "generic" can honestly mean

The question this page did not previously answer: is there one `SimVenue` that
simulates Coinbase and Nasdaq and CME, or one per venue?

**The answer is one generic continuous-matching core, and the `market.rs`
precedent is why.** That module exists because venue adapters normalise into a
shared vocabulary "so that a graph wired against one venue runs unchanged
against another"; §3.2's `Order` is the same lowest-common-denominator move on
the execution side, and §7 names one crate, `wingfoil-sim`, not a family. Fee
schedules and queue-position assumptions are then per-venue *parameters* over
that core.

**But the easy axis is parameters, and the hard axis is mechanism.** Fees and
queue position are settings. These are not:

- **Auctions.** Nasdaq's opening and closing crosses are a different matching
  algorithm from continuous trading, not a parameterisation of one — and for
  many equity strategies the closing cross is where the volume is. Coinbase
  has no auction at all.
- **Halts and bands.** LULD, odd-lot handling and Reg NMS order protection are
  Nasdaq-side concerns with no Coinbase analogue.
- **Priority.** Price-time is not universal: CME runs pro-rata on some
  products, so even the matching rule is venue-dependent rather than a constant
  the core can assume.
- **Order-level venue semantics.** Coinbase's self-trade-prevention modes are
  not expressible in a FIX-derived `Order` (§3.2), which is the first place the
  lowest-common-denominator choice costs something real.

So a single parameterised price-time matcher simulates the *intersection* of
those venues, not any one of them in full. That is a defensible scope, but it
must be stated, because "we can backtest Nasdaq" and "we can backtest Nasdaq's
continuous session, ignoring the crosses" are different claims — and §4.4's
conservative-by-default rule forbids the second from being presented as the
first.

**The in-tree precedent for handling the variation is `Sequencing`**, which
normalises "how a venue numbers its updates" across *the two shapes venues
actually use* — an enum in the neutral layer enumerating the small number of
real variants, rather than either a generic parameter space or a per-venue
fork. Matching priority and fee schedules should follow that shape, and it
subsumes §12's narrower question about where a fee schedule lives.

The scope ruling, then: **continuous matching is in scope and generic;
auctions, halts and price bands are explicitly out of scope until a venue a
user actually trades demands them**, at which point they arrive as named
mechanisms rather than as flags on the continuous matcher. P0 is unaffected —
fill-at-touch is venue-agnostic by construction — which is why this is P2's
central design question and not a blocker on proving the loop.

### 4.4 Conservative by default

Where a modelling choice is uncertain, the default is the one that makes the
backtest *worse*. A simulator that flatters is worse than no simulator,
because it produces confident numbers instead of no numbers. Any optimistic
setting is opt-in and named as such.

## 5. The loop, and what the engine already guarantees

### 5.1 `feedback` is `time + 1` — causality is structural

The hardest question in the design turns out to be already answered.
`Builder::feedback_send` pushes each value onto the paired source's queue at
**`time + 1`**, and the source emits it on the next cycle. Documented on both
the fluent and builder surfaces: *"values arrive on the source one cycle
later."*

So an order emitted at *t* cannot produce a fill the strategy observes at *t*.
The engine makes same-cycle causality violation unrepresentable, rather than
leaving it to the simulator's discipline. That is a significant de-risking of
the whole plan, and it is worth stating in the proposal precisely because it
is the sort of thing that would otherwise be discovered — or not — late.

The `+1` nanosecond is a *floor*, not a model. Realistic ack and fill latency
is a larger delay scheduled on the same mechanism (§4.2, decision 2).

**What it does not answer** is §4.2's decision 1. The floor protects the
strategy's side of the hop; the sim's own order-vs-book-update ordering is a
separate question that recurs one cycle later and that the engine takes no
position on. Do not read this subsection as having disposed of it.

### 5.2 Dedup, and why the burst shape defuses it

`TimeQueue` suppresses duplicate `(value, time)` pairs by design, and
`feedback` is built on it. The naive worry is that two identical orders in one
instant collapse into one — a silent, correctness-destroying bug.

The burst shape largely removes it. If orders travel as `Burst<Order>`, two
identical orders in one cycle live *inside one value*, and dedup cannot split
them; the burst rides through intact. The risk is confined to a scalar
round-trip through `feedback`. Order identity (§3.4) closes the remainder,
since two orders with distinct ClOrdIDs are never equal values.

This is not a reason to skip the test. Which shape the feedback edge actually
carries is exactly what phase 0's test pins, and pinning it is cheap.

### 5.3 The phase-0 test

One integration test, in the house style: `HistoricalFrom(NanoTime::ZERO)`,
`with_time()` + `accumulate()`, exact values and exact tick times. It asserts
the loop closes, that a fill lands strictly after its order, that a burst of
distinct orders in one cycle produces the fills of all of them, that an order
matches against the book as of the instant §4.2's decision 1 settles on — not
whichever one the wiring happens to produce — and that position folds correctly
over the result. If that test is green, the architectural claim in §2 is
established and everything above it is ordinary work.

## 6. FIX

### 6.1 The codec is the piece that is actually missing

`Order` ↔ NewOrderSingle and ExecutionReport ↔ `Fill`. It lives in tree beside
the types (§3.1). Today `fix_gw.rs` hand-rolls it with raw tag numbers, which
is fine for a showcase and unacceptable as the only path a user has.

This is also what makes the live half of the swap point one line of wiring
rather than a bespoke translation layer per venue — and it is the reason
`Order` is designed from the FIX side in §3.2.

Note what this does *not* require: no change to the FIX session engine. The
codec sits above `FixMessage`, which stays opaque and field-addressed.

### 6.2 A FIX-speaking sim venue is a different tool, and it already has a base

`fix_accept` / `fix_accept_with_options` exist today, and the `trading_e2e`
showcase already runs a FIX gateway. A simulated *acceptor* that answers
NewOrderSingle with ExecutionReports is genuinely useful — session-level
rehearsal, resend and GapFill behaviour under load, venue certification
practice.

It is the wrong thing to backtest through. A research loop that carries
sequence numbers, heartbeats and a session state machine pays for all of it
and learns nothing about the strategy. Keep the two separate: `SimVenue` for
backtests, the FIX acceptor for session testing.

## 7. Where the machinery lives — a ruling

`SimVenue`, the OMS, risk, portfolio and venue execution adapters are
**separate crates, out of this tree** — `wingfoil-sim`, `wingfoil-exec` and
venue crates, as the roadmap says. This repository stays an engine plus
adapters; it does not grow a trading platform inside it.

The in-tree surface this project adds is deliberately small: two types, their
enums, and a FIX codec — plus, unresolved, the position fold (§12: §8 assumes
it is bindable beside `Order`/`Fill`, P0 lists it as a deliverable, and this
ruling would put it out of tree). Decide that before P0 ships, not after. If
the surface starts growing an order state machine, the ruling is being
violated.

### 7.1 Where P0 itself lives

The ruling above is about the *published* machinery, and P0 is not that: its
`SimVenue` is deliberately dishonest (§4.1) and exists only to prove the loop
closes. Standing up a `wingfoil-sim` crate for it would publish a simulator we
have already said lies.

So **P0's simulator is a test fixture in `crates/wingfoil/tests/`, not a
crate.** That satisfies §5.3's requirement for a house-style integration test
with exact values and tick times, keeps a known-dishonest sim off the public
surface, and leaves §7's ruling intact for the real one. `wingfoil-sim` is
created at P2, out of tree, and P0's fixture is then deleted rather than
promoted — it has served its purpose once the architectural claim is
established.

The alternative — create `wingfoil-sim` at P0 and grow it — was rejected
because it puts a repository boundary between the loop and the test that
proves it, in the one phase whose entire deliverable is that test.

## 8. Python

The bindings are not an afterthought here. A large part of the audience for a
backtesting layer works in Python, and a strategy layer they cannot reach
loses most of the value. `Order`/`Fill` and the position fold should get
bindings in the same phase that stabilises them, following
[`bind-adapter`](../../../.claude/commands/bind-adapter.md). The sim venue
itself can follow later — the types are what a Python strategy needs first.

## 9. Gates, in order, each with an exit criterion

- [ ] **P0 — the loop.** `Order`/`Fill` in the `market.rs` idiom, a
  position/PnL fold, a fill-at-touch `SimVenue` as a test fixture (§7.1), a
  **named reference strategy** — the simplest passive thing that quotes and
  gets filled, and the baseline every later gate measures against — and the
  §5.3 test. Exit: the test is green, and it pins both the feedback edge's
  carried shape and §4.2's order-vs-update ordering.
  **A loop that cannot be closed cleanly ends the project here**, and the
  finding is worth more than the code.
- [ ] **P1 — the FIX codec.** `Order` ↔ NewOrderSingle, ExecutionReport ↔
  `Fill`, in tree beside the types. Exit: `trading_e2e`'s `fix_gw.rs` drops
  its hand-rolled tag assembly and uses it, unchanged in behaviour.
- [ ] **P2 — an honest fill model.** Queue position, fees, configurable
  ack/fill latency, partial fills, the §4.2 decisions documented, and the
  §4.3 scope ruling on which venue mechanisms are in. Out of tree, as
  `wingfoil-sim`. Exit: **P0's reference strategy**, re-run unchanged against
  this simulator, shows materially worse PnL than against P0's fixture, and
  the difference is attributable to named decisions (queue position, fees,
  latency) rather than unexplained.
- [ ] **P3 — Python bindings** for the types and the fold.
- [ ] **P4 — OMS, risk, portfolio.** Only when a real strategy demands them.
  Deliberately unspecified here; the roadmap's instruction to defer stands.

## 10. What would make this not worth doing

- **P0 shows the loop needs kernel changes.** The roadmap's structural claim
  is that every gap is "an adapter, an op, or ops-tooling" and that the engine
  core needs no rework. If closing the order→fill loop turns out to need
  changes to the kernel, `TimeQueue` or the tiers, that claim is false and the
  project should stop and be redesigned rather than proceed.
- **The fill simulator turns out to be the product.** If P2 grows past a
  bounded, documented model into an open-ended matching-engine effort, we are
  building the thing the roadmap explicitly declines to build (parity with an
  incumbent platform's breadth). §4.3's scope ruling is the guard here: the
  first flag added to the continuous matcher to fake an auction is the signal
  this has started.
- **Nobody wires a *real* strategy to it.** This layer's value is entirely
  realised by someone running a strategy they care about end to end. P0's
  reference strategy (§9) does not count — it is a test instrument, and it
  exists to give P2 a measurable baseline, not to demonstrate demand. If P0
  and P1 land and no real strategy follows, P2 onward should not be started.

## 11. Sequencing against the roadmap

The roadmap's §5 originally put trading semantics (item 6) *after* feed
coverage (items 4–5: `mold_itch`, SBE). **It now carries the inverted order,
on the argument below.**

The original rationale was that listed-markets data is a prerequisite for a
credible fill simulator. That holds for a *credible* one, but not for the
loop: `SimVenue` needs *a* book, not a *multicast* book, and the existing
`csv`, `ws` and FIX paths already produce `MarketEvent` streams that
`OrderBook` consumes. Phase 0 as scoped here has no dependency on either
adapter.

The cost of the roadmap ordering is that the piece which makes the stack
demoable end to end waits two quarters behind adapter work it does not need.
The cost of inverting is that P2's fill model is tuned against thinner data
than it eventually deserves — which is recoverable, and is P2's problem rather
than P0's.

So the split the roadmap now records: the loop (P0) and the FIX codec (P1)
come before feed coverage; P2's fill model comes after it, tuned against real
depth. P0 and P1 should not wait for `mold_itch`; P2 should.

## 12. Open questions

- [ ] Scalar or burst on the strategy's *order* edge — §3.3 argues burst on
  the fill side is forced, but the order side is a genuine choice, and it
  changes the dedup analysis in §5.2. Pin it in P0's test.
- [ ] Does `Order` carry `InstrumentId` inline or behind an index? Note
  `InstrumentId` is *already* two interned `Sym`s (`market.rs`), so "interned
  or not" is not the choice — the choice is whether the execution path is hot
  enough to want a narrower handle than the pair.
- [ ] Where order *state* lives — inside the sim, in a separate OMS op, or as
  a fold the strategy owns. P0 can sidestep this; P4 cannot.
- [ ] Does the position fold belong in tree (it is small, general, and has no
  dependencies) or out with the machinery? The §7 ruling says out; the fold's
  triviality argues in, and §8 assumes in by binding it beside the types.
  **This one blocks P0**, because P0 ships the fold — it cannot be deferred
  the way the rest of this list can.
- [ ] Whether `Traced<T, L>` should wrap orders and fills by default, so the
  execution hop is stamped like every other hop in the showcase.
- [ ] Fee schedules and matching priority: a trait each venue crate
  implements, or `Sequencing`-style enums in the neutral layer enumerating the
  variants venues actually use? §4.3 argues for the latter; the shape is P2's
  to settle.
- [ ] Which venue P2 tunes against first, since §4.3's scope ruling defers
  auction and halt mechanics until a specific venue demands them — and that
  choice decides which ones arrive first.
