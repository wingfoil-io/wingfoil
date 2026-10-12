# The execution layer

A map of `adapters::execution` for someone reading or integrating against it:
the main types, how they connect, and which node drives each one. The module
docs hold the arguments, and
[`execution/CLAUDE.md`](../crates/wingfoil/src/adapters/execution/CLAUDE.md)
holds the rules for changing the code.

## What it is

The execution layer sits between a strategy and a venue, and it knows neither
an asset class nor a venue. It connects to nothing. A venue integration
implements one trait, `Venue`, out of tree. A strategy emits what it wants
showing, as `Desired`s, and reads back fills, positions and a kill switch's
stance. Between the two are the order edge, an OMS, parent orders, a position
fold, a reconciler, a kill switch and a per-order ceiling.

The instrument is the caller's type: everything is generic over
`I: Instrument`, a blanket alias for `Copy + Eq + Hash + Ord + Default +
Debug`, and nothing here reads it. Anything that has to *know* the
instrument, such as a valuation or a size cap, takes a closure or a value
from the caller.

## The two edges

The swap point has one edge out and one edge back, both in
[`edge`](../crates/wingfoil/src/adapters/execution/edge.rs):

| Edge | Type | Variants |
|---|---|---|
| out, strategy to venue | `Burst<Request<I>>` | `Place(Order)`, `Amend(Amend)`, `Cancel(ClientOrderId)`, `CancelAll` |
| back, venue to strategy | `Burst<Report<I>>` | `Ack`, `Reject`, `Fill`, `Cancelled`, `Expired`, `Triggered` |

Both are `Copy`, because a request goes round a feedback edge where a value
that allocates would be cloned on every hop. Same-instant requests ride one
burst, as do same-instant reports, and every reader applies a burst whole, in
order.

`Order` is an intent to open something, and that is all it can say. The OMS
also has to say "stop showing that" and "show it a tick lower", and a kill
switch opens with cancel-all, so the outbound edge carries a `Request`.
`Amend` is first-class: an absolute price and quantity on one order id, plus
the instrument so a per-order cap can judge it alone. `CancelAll` takes no
arguments, because it is what a kill switch sends when there is no state
left to trust. Reports are one stream so an ack cannot arrive after the fill
it precedes. The defaults are inert: a defaulted `Request` is a
zero-quantity `Place` that fails validation, and a defaulted `Report` is a
`Reject` naming no order.

Requests out and reports back form a cycle. The caller's graph breaks it
with **one feedback cut on the request wire**, wired in one place by
`GraphBuilder::feedback_loop` — the closure is handed what the OMS sent and
returns the requests to send round — so
the venue sees a burst one engine instant after the OMS emitted it, which is
where an order's trip belongs. A `Venue` never wires a cut of its own.

## The vocabulary

These live in [`order`](../crates/wingfoil/src/adapters/execution/order.rs),
[`exec_id`](../crates/wingfoil/src/adapters/execution/exec_id.rs) and `edge`.

- **`Order`.** Id, instrument, `Side`, `Qty`, an optional `Px`, `kind`,
  `tif`, `reduce_only`, an optional `Trigger`, and `created`. Build one
  through `Order::limit`, `Order::post_only` or `Order::market`, which cannot
  produce a `kind`, `price` and `tif` that disagree. `Order::validate` is the
  backstop for an order assembled any other way.
- **`OrderKind`** is `Limit`, `PostOnly` (the default, the one that cannot
  cross) or `Market`. **`TimeInForce`** is `GoodTillCancel`,
  `ImmediateOrCancel`, `FillOrKill` or `Day`. A **`Trigger`**
  (`Trigger::stop` or `Trigger::take` on a `Reference`: `Mark`, `Index`,
  `Last`) holds an order back until the reference reaches its price.
- **`ClientOrderId`.** An `Epoch` (18 bits) above a 32-bit sequence. The
  epoch is the process's, handed in by the caller, so a restarted process
  never mints the last one's numbers; `Epoch::ZERO` is the bare counter a
  backtest runs on. The top bit of the sequence, `ClientOrderId::RESERVED`,
  is held for another minter in the same process. The OMS never mints there
  and passes over a report naming one.
- **`ExecId` and `VenueId`.** The venue's ids for an execution and for an
  order: two inline, bounded types, so neither stands in for the other.
  `ExecId::new` refuses what exceeds `ExecId::CAPACITY`; it never truncates.
- **`Fill`.** One execution: `order`, `exec_id`, `instrument`, `side`, `qty`
  (this execution), `filled` (the order's total so far, FIX `CumQty`),
  `remaining` (FIX `LeavesQty`), `price`, `fee` (negative is a rebate),
  `liquidity` (`Liquidity::Maker` or `Taker`), `venue_time` and `recv_time`.
  `recv_time` is engine time from `Ctx::time`, never the wall clock.
- **`Cashflow`.** Money moved on a position with nothing traded: funding, a
  coupon, a dividend. Positive is money in. Not a future's variation margin,
  which the mark already holds.
- **`Holdings`** is a `Burst<Held>` of the venue's positions, with `whole`
  saying whether it is the whole book (anything unnamed is flat).
- **`TradingState`** is `Open`, `Auction`, `Halted` or `Closed` (the
  default). A continuous venue never states one, and the OMS starts open.
- **`Account`** is the venue's `equity` and `utilisation`, both `Option`;
  `None` is never a pass. **`MmpTrip`** says the venue's market maker
  protection pulled the quote book, until `frozen_until`.

Money on this layer is carried as `Qty` in the settlement currency: a fill's
fee, a cashflow, an account's equity, every field of a `Pnl`. The rule for
its arithmetic is `market`'s operators and `mul_div`, multiplying before
dividing, with the scale never spelled out at a call site and never `f64`
for a value that is carried on as money.

## Deciding: `Desired`

A strategy driving the OMS does not send orders. It states what it wants
showing on one instrument, as of one instant, and the OMS closes the gap.
That statement is a `Desired`, in [`oms`](../crates/wingfoil/src/adapters/execution/oms.rs):
an instrument, a `Ladder` of bids and one of asks, an `as_of`, an `Intent`
and `reduce_only`.

- **`Ladder`.** Up to `MAX_DEPTH` (eight) `Level`s a side, best first,
  strictly monotone away from the touch. `Ladder::one`, `Ladder::none`,
  `Ladder::maybe` and `Ladder::from_levels` build one; the last refuses two
  levels at one price. `Level` is `market`'s price and quantity.
- **`Intent`.** `Rest` shows the levels, post-only and good-till-cancelled by
  default. `Cross` takes rank 0 as a limit, immediate-or-cancel, at the
  level's price as a cap. `Trigger(Trigger)` rests one level untriggered
  until the trigger fires. A triggered desired is a decision of its own on
  the instrument, remembered beside the plain one, never over it.

Build one with the constructor for its shape, then `.reduce_only()` if it
is. Every constructor takes the `as_of`, the engine time of the decision, and
none defaults it: the OMS keys answers and staleness on it, so a refused
level is held until a newer `as_of`, and a cross is spent once per `as_of`
and price.

| Constructor | What it says |
|---|---|
| `Desired::quote(instrument, as_of, bids, asks)` | a ladder each side, rested |
| `Desired::two_way(instrument, as_of, bid, ask)` | one level each side, rested |
| `Desired::rest(instrument, as_of, side, ladder)` | one side rested, nothing on the other |
| `Desired::cross(instrument, as_of, side, level)` | take one level, capped |
| `Desired::stop(instrument, as_of, side, trigger, level)` | one level under a trigger |
| `Desired::nothing(instrument, as_of)` | withdraw the plain decision |
| `Desired::no_trigger(instrument, as_of)` | withdraw the triggered decision |

**A burst of desireds is not the whole book.** A key absent from a burst
wants what it wanted last time. A quoter and a hedger decide on separate
edges, and a burst carrying one must not cancel the other. What withdraws a
silent key is `Config::max_desired_age`, judged at engine time, so a stalled
graph withdraws rather than resting on a strategy that has stopped thinking.
An explicit `Desired::nothing` is acted on at once.

## The OMS

`Oms<I>` is the diff and the state machine, a pure fold with no venue, no I/O
and no clock but the engine time handed in. Two invariants are the component:

1. **One request in flight per slot.** A slot with a request outstanding
   emits nothing; the desired is remembered and the diff re-runs when the
   report lands.
2. **Nothing is emitted that matches what is working.** Re-pricing as the
   touch moves is the strategy emitting a `Desired` every tick.

**Slots.** Each instrument holds up to `MAX_DEPTH` slots a side, and each
slot is a `Slot`: `Idle`, `Pending`, `Working`, `PendingAmend` or
`PendingCancel`, each holding a `Placed` (id, price, qty, `remaining`,
`as_of`, trigger). The diff matches wanted levels to slots by price, then by
rank; a slot's index names where a report routes, not a rank. A slot
compares against what is still showing, so a part-filled quote is amended
back up, and it works the `price` and `remaining` the venue's `Ack` names.

**`Config`.** No `Default`. Start from `Config::unmetered()` and state what
differs: `with_rate`, `with_ratio`, `with_passive`, `with_lifetime`,
`with_max_desired_age`, `with_retake` and `with_min_requote`. A live venue
is `Config::unmetered().with_rate(VENUE_RATE)`. `Passive` is what a resting
level is sent as: `PostOnly`, or `Limit` on a venue with none, where the
price guard becomes the strategy's. `Lifetime` is `GoodTillCancel` or `Day`.
`retake` spaces two crosses on one instrument, so an IOC the venue killed
does not wake a fresh cross every instant.

**The budget.** [`rate_limit`](../crates/wingfoil/src/adapters/execution/rate_limit.rs)
holds the venue's order-entry limits as stated: an `OrderRate` of two
`Terms` (trading, and cancel-all) and a headroom. `OrderRate::stated` is the
`const` form, a compile error on bad terms; `OrderRate::new` is the fallible
one; `OrderRate::UNMETERED` says the venue states none. `OrderRate` has no
`Default`. The OMS spends `floor(rate × headroom)` through two engine-time
`Bucket`s. When tokens are short it pays a cancel-all first, then cancels,
amends and places. What waits is re-planned on the next diff, never queued
— and the node wakes itself for that diff at the instant a token is back —
so a plan that changed before its token arrived is dropped as superseded.
`Pacing` (`deferred`, `superseded`, `waiting`) counts both. A venue's
message-to-trade `Ratio` holds places and amends, never a cancel.

**Refusals.** A post-only reject is ordinary: the touch moved. The OMS holds
the refused level until a desired with a newer `as_of` than `Placed::as_of`
arrives, because re-sending it is the same question with the same answer; a
refused amend is held the same way. `Oms::rejected` counts what the venue
rejected, `Oms::refused` the levels the OMS would not send, and
`Oms::adjusted` the acks that accepted something other than the ask.

**Cancel-all and the trading state.** `Oms::cancel_all` answers
`Request::CancelAll`, which the node puts at the head of the instant's burst,
so the venue pulls everything before it sees what the same instant places.
While `TradingState` is not `Open`, the OMS sends cancels and nothing else;
places and amends wait, unspent, for the open.

### The node

A graph drives the OMS through
[`OmsOps`](../crates/wingfoil/src/adapters/execution/oms/node.rs), and only
through it, on a `Stream<Burst<Desired<I>>>`:

```rust
let (requests, pacing) = desired
    .wire_oms(config, epoch, &reports)
    .trading(&trading_state)  // optional
    .cancel_all(&kill)        // optional
    .build();
```

`.reading(..)` adds a third stream, a look at the `Oms` after each diff, and
`.sweep(..)` an optional clock. `OmsOps::oms` and `OmsOps::oms_reading` take
every input positionally and wire the same node, `OmsOp`. An input left out
never ticks.

**The node needs no clock.** Three things change what the OMS sends with
nothing new arriving: a remembered desired going stale, a retake ending,
and a token refilling while a request waits. After every diff the node arms
one wake-up at the earliest of them, `Oms::next_deadline`, so a stale quote
is pulled at the instant it stales and a held request goes the instant its
token is back. It books a new wake-up only when the deadline moves earlier,
so a strategy restating every tick books nothing new: its one wake-up
fires early, at most once per `max_desired_age`, and re-books.

Within an instant: reports (`Oms::apply`), the trading state, cancel-all,
the desireds, the sweep; then `Oms::diff` at the engine time, on every cycle
the node runs, and the next wake-up armed. The requests tick only on a cycle
that sent any.

## Parent orders

[`algo`](../crates/wingfoil/src/adapters/execution/algo.rs) is a layer above
the OMS, and the OMS does not know it exists. A `Parent` is a quantity worked
over time in one `Style`: `Twap` clips, an `Iceberg` restated every beat, or
one capped `Sweep`. Build one with `Parent::twap`, `Parent::iceberg` or
`Parent::sweep`, then `.reduce_only()` if it is. A parent emits `Desired`s
and reads fills back, adding no request kind, slot state or tag on `Order`
or `Fill`: every fill on its instrument and side while it is live is its
own. `Working` is one parent and `Algos` the keyed fold over them, driven
through `AlgoOps::algo(&landed, &reports, &clock)` on a
`Stream<Burst<Parent<I>>>`. It returns desireds for the OMS node and a
`Burst<Progress>` (`filled`, `remaining`, `clips`, `status`). Within an
instant: landed requests, reports, new parents, the clock; then
`Algos::decide`. It sits on the forward side of the cut and adds none.

## The venue side

[`venue`](../crates/wingfoil/src/adapters/execution/venue.rs) holds the swap
point. `Venue<I>` has one method, `wire(&self, requests) -> Session<I>`, and
the strategy graph cannot tell which implementation it was handed. A
`Session` is seven forward edges, one per audience:

| Stream | What rides it | Who reads it |
|---|---|---|
| `reports` | `Burst<Report>`: what became of the requests | the OMS, and the fills for the position fold |
| `settlements` | `Burst<Fill>`: positions the venue closed at its cut | the position fold and the reconciler; never the OMS |
| `account` | `Account`: equity and margin utilisation | the risk limits |
| `holdings` | `Holdings`: the venue's own positions | the reconciler |
| `cashflows` | `Burst<Cashflow>`: money moved with nothing traded | the position fold |
| `mmp` | `Burst<MmpTrip>`: the venue pulled the quote book | the risk limits |
| `trading` | `TradingState`: whether the book takes orders | the OMS |

A venue builds its session as `Session::quiet(reports)` plus a `with_*`
setter per stream it states (`with_settlements`, `with_trading` and so on).
A settlement is not a `Report`: it answers no order the OMS sent.

**The ceiling.** [`ceiling`](../crates/wingfoil/src/adapters/execution/ceiling.rs)
holds `Capped`, a `Venue` around a venue. `Capped::new(&venue, cap)` judges
every place and amend against the caller's `Cap` for its contract (`Judge`,
stateless, reading an `Amend`'s own instrument), and the first one over, or
on a contract with no cap, aborts the run with a `Breach`. It never drops or
resizes: a dropped request is one the OMS waits on forever. It wraps a
simulated venue too, so a backtest trips where live would.

**FIX.** [`fix`](../crates/wingfoil/src/adapters/execution/fix.rs) is the
shape FIX imposes on order entry, as values: `Message` (`New`, `Replace`,
`Cancel`, `MassCancel`), each with its own `ClOrdId`, and `ExecReport`
answering one by id. `ReplaceChain` adapts the OMS's edge to that shape. It
owns the map from each `ClientOrderId` to the venue's current `ClOrdId`, so
**the OMS never learns a venue id**. A replace states the venue's `CumQty`
plus the amend's size as its total `OrderQty`; a cancel-all goes out per
order on a venue with no mass cancel; a request for an order the chain no
longer holds is answered at once as `UnknownOrder`. A graph drives it
through `FixOps` (`requests.replace_chain(mass_cancel, epoch, &executions)`):
executions first, then requests.

**The test venue.** [`testing`](../crates/wingfoil/src/adapters/execution/testing.rs),
behind `execution-testing`, holds `FixVenue`: a FIX-shaped harness, not a
model. It matches against a `Touch` the test states, with unlimited depth
behind it, and its capabilities are a `Profile` (`post_only`, `mass_cancel`;
`Profile::EXCHANGE` has neither). `SimVenue::new(profile, epoch, &touch)` is
the same harness as a `Venue`, with `.with_trading(..)` for a session
venue: one node holding the `ReplaceChain` and the `FixVenue`, because the
venue answers in the instant it is sent to. Within an instant: trading,
touch, then requests. It states no holdings.

## Positions and risk

**The fold.** [`position`](../crates/wingfoil/src/adapters/execution/position.rs)
holds `Book<I>`, a fold over fills keyed by instrument, with no formula for
any contract. `Book::new` takes the caller's valuation, a closure giving each
instrument a `Measure` (`Linear`, `Scaled` through `Measure::scaled`, or
`Inverse`) or `None`, and PnL is always `net × measure(mark) − cost`. A
`Position` is one instrument's net, cost, realised, fees and cashflows; a
`Pnl` is `realised` (gross of fees), `unrealised`, `fees` and `cashflows`.
`Book` has no `Default`: a book that values nothing is asked for by name.

**Never withhold a net, sometimes withhold a value.** A quoter that thinks it
is flat keeps selling, so the net is always folded. The money is not: an
instrument with no `Measure` answers `None` for its PnL, and
`Book::unvalued` counts the fills that moved it. Currencies are not added
together. `Book::total` and the free `position::total` take one settlement
currency (a predicate) and a mark per instrument, and answer `None` where an
open position has no mark.

**Reconciliation.** [`reconcile`](../crates/wingfoil/src/adapters/execution/reconcile.rs)
holds `Reconciler<I>`, which reads a `Fold` (a `Book` and a mark to re-base
at) against the venue's `Holdings`. **The venue is the truth.** A
difference that outlives `Config::grace` is a `Mismatch` (`instrument`,
`ours`, `venue`, `since`), and the fold is re-based on the venue through
`Fold::rebase`. Sizes compare exactly; there is no tolerance. Nothing is
judged before the venue first speaks. The caller latches its risk on every
`Mismatch`.

**The kill switch.** [`kill_switch`](../crates/wingfoil/src/adapters/execution/kill_switch.rs)
holds `Switch<L>`, a latch over any set of limits. `L: Limit` is the
caller's, usually a fieldless enum listing itself in `Limit::ALL`.
`Breaches<L>` is a `Copy` bit set of them. The caller's assessment reads
every cap through `kill_switch::within`, so a `NaN` is outside, and records
an input it could not evaluate as a breach. `Switch::daily_loss` measures
against the day's opening equity. `Stance` is `Open` or `Halted` (the
default).

**A breach latches, clear refuses, a restart carries.** Once `Switch::record`
sees a breach, the stance is `Halted` until an operator's `Switch::clear`,
which refuses while a breach still stands. `Switch::snapshot` gives a
`Snapshot` (the latch, what stood, the day's opening equity), and
`Switch::restore` takes it back after a restart.

### The nodes

- **`PositionOps`**: `fills.wire_position(measure)`, optional
  `.settlements(..)`, `.cashflows(..)` and `.reading(..)`, then `.build()`.
  Within an instant: fills, settlements, cashflows, then the reading
  (`read_positions` by default).
- **`ReconcileOps`**: `fills.wire_reconcile(config, measure, &holdings)`, a
  required `.sweep(..)`, optional `.settlements(..)`, `.cashflows(..)`,
  `.marks(..)` and `.reading(..)`, then `.build()`. Within an instant: fills,
  settlements (also `Reconciler::settled`), cashflows, every `Holdings` in
  the burst in order, marks, the sweep; then `Reconciler::reconcile`. It
  emits the instant's mismatches and a reading of the `Desk` (`Standing` by
  default).
- **`KillSwitchOps`**: `readings.kill_switch(&clear, day_start, restored,
  assess)`. Within an instant: every reading assessed and recorded in order,
  then the operator's clear, so a clear in the instant of a breach is
  refused; the refusal rides `Assessed::cleared`. `StanceOps::stance` takes
  the `Stance` alone.

`ReconcileOps` is one node holding the book and the reconciler because the
reconciler's `Fold` *is* the book: two nodes would share it through a
`RefCell` on the graph path. A caller that reconciles wires it instead of
`PositionOps`. The re-base mark is the `marks` input, because a mark moves.

## Types, nodes and examples

Each stateful type has one node and one fluent trait, and a graph reaches it
only through that trait. The examples are under
[`examples/adapters/execution/`](../crates/wingfoil/examples/adapters/execution/README.md).

| Type | Node | Fluent trait | Example |
|---|---|---|---|
| `oms::Oms` | `OmsOp` | `OmsOps` | `oms_quote`, `message_ratio`, `exchange_day`, `strategy_loop` |
| `algo::Algos` | `AlgoOp` | `AlgoOps` | none yet; unit tests in `algo/node.rs` |
| `fix::ReplaceChain` | `ReplaceChainOp` | `FixOps` | none yet; unit tests in `fix/node.rs` |
| `testing::FixVenue` + `ReplaceChain` | `SimVenueOp` | `Venue` (`SimVenue`) | `replace_chain`, and every example above that runs the OMS |
| `position::Book` | `PositionOp` | `PositionOps` | `position_book` |
| `Book` + `reconcile::Reconciler` | `ReconcileOp` | `ReconcileOps` | `reconcile`, `strategy_loop` |
| `kill_switch::Switch` | `SwitchOp` | `KillSwitchOps`, `StanceOps` | `latch_and_cap`, `strategy_loop` |
| `ceiling::Capped` (stateless, through `Judge`) | none: a filter on the request edge | `Venue` (`Capped`) | `latch_and_cap`, `strategy_loop` |

`order_edge` is the one example off a graph: its subject is the vocabulary.

## The whole loop

[`strategy_loop`](../crates/wingfoil/examples/adapters/execution/strategy_loop/)
is the layer in one graph, and the wiring a venue integration copies. A toy
quoter rests one lot a tick outside the touch on one future. Each wire hop is
a `delay(WIRE)`; the venue's holdings are a scripted stream, because
`SimVenue` states none.

```text
 touches ---------------------------------------------+
    | delay(WIRE): the touch the quoter sees          |
    v                                                 |
 quoter <------ stance (read passively) ---------+    |
    | Burst<Desired>                             |    |
    v                                            |    |
 OmsOps <------ cancel_all (stance turns Halted) +    |
    ^   | Burst<Request>                         |    |
    |   v                                        |    |
    |  returned by the feedback_loop closure     |    |
    |  ======== THE ONE CUT (next instant) ===== |    |
    |  landed, delay(WIRE)                       |    |
    |   v                                        |    |
    |  Capped (Clip: five lots an order)         |    |
    |   v                                        |    |
    |  SimVenue (FixVenue + ReplaceChain) <------|----+
    |   | Session.reports, delay(WIRE)           |
    +---+ reports                                |
        | fills only                             |
        v                                        |
 ReconcileOps <-- holdings, settlements,         |
    |             marks (the touch's mid), sweep |
    | DeskReading: net, mark, PnL, mismatches    |
    v                                            |
 KillSwitchOps <-- clear (the operator)          |
    | Assessed, .stance()                        |
    +--------------------------------------------+
```

Closing the loop through the switch needs no second cut. The switch reads
the reconciliation node, which reads the venue's reports, and those are
downstream of the cut's source. Its stance feeds the quoter and the OMS,
which are upstream of the cut's sink. Every path from the OMS back to itself
passes through the one cut, so the graph stays acyclic.

**Everything here is engine time.** The OMS's diff, `as_of`, the budget's
buckets, `max_desired_age`, the OMS's armed wake-ups, the reconciler's grace,
the switch's day and every `recv_time` are `Ctx::time`, never the wall clock. That is what makes a
replay deterministic: the same inputs at the same instants send the same
requests and spend the same tokens. The keyed state walks in the
instrument's order for the same reason.

## Feature flags and where things live

| Feature | Gives |
|---|---|
| `execution` | the whole layer; implies `market` |
| `execution-testing` | `testing::FixVenue` and `testing::SimVenue`; implies `execution` |

Both are off by default, and the layer is out of the prelude: import by
module path, as in `use wingfoil::adapters::execution::oms::OmsOps;`.

| Module | Holds |
|---|---|
| `execution::edge` | `Request`, `Report`, `Amend`, `Ack`, `Reject`, `Account`, `Holdings`, `Held`, `Cashflow`, `MmpTrip`, `TradingState` |
| `execution::order` | `Order`, `Fill`, `OrderKind`, `TimeInForce`, `Trigger`, `ClientOrderId`, `Epoch`, `Instrument` |
| `execution::exec_id` | `ExecId`, `VenueId` |
| `execution::oms` | `Oms`, `Desired`, `Ladder`, `Intent`, `Config`, `Passive`, `Lifetime`, `Slot`, `Pacing`, `OmsOps` |
| `execution::rate_limit` | `OrderRate`, `Terms`, `Ratio`, `Bucket` |
| `execution::algo` | `Parent`, `Style`, `Progress`, `Algos`, `AlgoOps` |
| `execution::venue` | `Venue`, `Session` |
| `execution::ceiling` | `Capped`, `Cap`, `Judge`, `Breach` |
| `execution::fix` | `ReplaceChain`, `Message`, `ExecReport`, `ClOrdId`, `FixOps` |
| `execution::position` | `Book`, `Position`, `Measure`, `Pnl`, `total`, `PositionOps` |
| `execution::reconcile` | `Reconciler`, `Fold`, `Config`, `Mismatch`, `ReconcileOps` |
| `execution::kill_switch` | `Switch`, `Limit`, `Breaches`, `Stance`, `Snapshot`, `within`, `KillSwitchOps`, `StanceOps` |
| `execution::testing` | `FixVenue`, `SimVenue`, `Profile`, `Touch` |

## Where to read next

- The module docs under
  [`adapters/execution/`](../crates/wingfoil/src/adapters/execution/mod.rs),
  `oms` first, then `edge`, `order`, `venue`, `position`, `reconcile`,
  `kill_switch`, `algo`, `fix`, `ceiling`, `rate_limit`, `exec_id` and
  `testing`. Each node's in-instant order is argued in its `node` module.
- [`execution/CLAUDE.md`](../crates/wingfoil/src/adapters/execution/CLAUDE.md)
  for the rules a change to the layer must keep.
- [`market/CLAUDE.md`](../crates/wingfoil/src/adapters/market/CLAUDE.md) for
  the `Px`, `Qty` and `Level` vocabulary the layer builds on.
- The [examples group README](../crates/wingfoil/examples/adapters/execution/README.md),
  starting with `strategy_loop`.
- [`planning/execution-on-graph-plan.md`](planning/execution-on-graph-plan.md)
  for how the examples and the library nodes came to be shaped this way.
