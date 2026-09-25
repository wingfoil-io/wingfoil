# execution (wingfoil)

The venue-neutral **order and fill vocabulary**, the position/PnL fold, and a
fill-at-touch simulated venue. The execution-side counterpart to
[`market`](../market/CLAUDE.md): that module is what venue adapters normalise
market data *into*; this one is what a strategy emits orders *in*.

**wingfoil-only** — there is no legacy twin, so there is no parity oracle and
no `port-plan.md` row. The design body is
`docs/planning/proposals/trading-stack.md` (**Project Venue**); what is built
here is its gate **P0**, and the section numbers cited below are that page's.

## Layout

| File | What | Feature |
|---|---|---|
| `mod.rs` | `Order`, `Fill`, `OrderType`, `TimeInForce`, `ClOrdId`, `ExecId`, `Notional`, the fixed-point helpers | `execution` |
| `position.rs` | `Position` + the `position` / `position_bursts` ops + `PositionOps` | `execution` |
| `sim.rs` | `SimVenue`, the matcher, `SimVenueOps::sim_venue` | `execution-sim` |
| `exchange.rs` | `ExchangeOp` (participant CLOB), `Request`/`Report`, fees, `Ledger`, `ExchangeOps`/`ExchangeOutputOps`/`LedgerOps` | `execution-exchange` |

`execution = ["market"]` — the types are built on `Px`/`Qty`/`InstrumentId`/
`Side`, so the gate implies `market`. `execution-sim = ["execution"]`, separate
because a *live* graph needs the types and the fold and has no use for a
simulator. `execution-exchange = ["execution"]`, separate from
`execution-sim` because they model different things: `sim` fills one strategy
against a *replayed* book; `exchange` has no replayed book and matches
participants against each other.

## Entry points

| Surface | Shape |
|---|---|
| `PositionOps::position()` | `Stream<Fill>` **or** `Stream<Burst<Fill>>` → `Stream<Position>` |
| `exchange::ExchangeOps::exchange(cfg)` | `Stream<Burst<Request>>` → `Stream<ExchangeOutput>` (reports + public `MarketEvent`s) |
| `exchange::ExchangeOutputOps::{reports, market_events}` | split an `ExchangeOutput` stream |
| `exchange::LedgerOps::ledger()` | `Stream<Burst<Report>>` → `Stream<Arc<Ledger>>` (per-account positions) |
| `sim::SimVenueOps::sim_venue(&orders)` | `Stream<Arc<OrderBook>>` × `Stream<Burst<Order>>` → `Stream<Burst<Fill>>` |

Both are extension traits, out of the prelude, per the adapters convention.
There is no source and no sink here: like `market` and `augurs`, this adapter
connects to nothing and is transform ops only.

## The exchange's gotchas

- **Participants get `Report::Rejected`, never an `Err`.** One bad request
  must not stop a venue other accounts trade on. An `Err` out of
  `ExchangeOp` means the exchange is broken (fixed-point overflow), nothing
  else. Keep it that way when adding validation.
- **Reports for *all* accounts ride one burst**, in the order they happened —
  a maker's fill precedes the taker's for the same match. Route by
  `Report::account()`.
- **`AccountId` lives in `mod.rs`, not `exchange.rs`** — it is FIX tag 1 and
  belongs with the vocabulary, not the simulator.
- **Scope is ruled in `trading-stack.md` §13.** The first auction flag,
  hidden/iceberg/stop order or pro-rata allocation added here is the §10
  warning sign; take it to that section first.

## The gotchas that bite

- **`Order`/`Fill` are designed from the FIX side, not the simulator side**
  (§3.2). `cum_qty`, `leaves_qty` and `transact_time` are on the types because
  the P1 codec will need them. Do not trim a field because the simulator does
  not read it.
- **The order edge carries `Burst<Order>`** — §12's one genuinely open question
  on that side, settled in P0 and pinned in `tests/execution_loop.rs`. The fill
  edge being a burst is *forced* (one order crossing several levels fills
  several times at one instant), not a choice.
- **An order matches the book as of the previous instant** (§4.2 decision 1).
  `SimVenueState::book` holds the previous cycle's book and `cycle` adopts this
  cycle's only *after* matching. The engine does not settle this — `feedback`'s
  `time + 1` protects the strategy's side of the hop, and the question recurs
  one cycle later inside the sim. Inverting it makes
  `an_order_cannot_fill_against_the_update_it_arrived_with` fail, which was
  checked by actually inverting it.
- **`Side` has no `Default`**, deliberately (`market` is right not to claim
  one), so `Order` and `Fill` hand-write their `Default` for the engine's value
  slot. Do not "simplify" them to a derive by adding `#[default]` to `Side`.
- **The fill model is a known lie** and says so at the top of `sim.rs`: fill at
  touch, no queue position, no fees, no market impact, no modelled latency
  beyond `feedback`'s structural 1ns. Taking liquidity does not remove it from
  the replayed book. All of that is gate P2's to fix, and the docs must keep
  saying so until it does.
- **`ClOrdId`/`ExecId` are `Sym` but are *not* interned**, and must not be:
  they are unique by construction, so an interner over them grows without
  bound and shares nothing. `Sym::new` is a fresh allocation — so creating an
  id allocates (twice, with the `format!` callers use) even though cloning one
  does not. That is once per order and once per fill on the graph path;
  `InstrumentId` is the opposite case and rides on every message for an atomic
  increment.
- **`Notional` reuses `market`'s `fixed_point!` macro** rather than growing a
  second fixed-point implementation. That is why `market.rs` exposes
  `pub(crate) use fixed_point;` and `pub(crate) fn parse_fixed` / `fmt_fixed` —
  the macro body names the last two unqualified at the expansion site, so
  `mod.rs` imports them even though nothing else in the file calls them.
- **Money arithmetic goes through `notional_of` / `scaled_mul`**, which is the
  one place a scale factor can be got wrong. Both truncate towards zero and
  report overflow rather than wrapping.

## Not built yet

Gate **P1** is the FIX codec (`Order` ↔ `NewOrderSingle`, `ExecutionReport` ↔
`Fill`), which belongs in `codec.rs` here behind `execution` + `fix`, and whose
exit criterion is that `examples/showcase/trading_e2e`'s `fix_gw.rs` drops its
hand-rolled tag assembly for it. Gate **P4** is the OMS and risk, behind an
`execution-oms` feature, and is deliberately unspecified until a real strategy
demands it.

**The guard on scope creep is mechanical** (§7.3): *the default-feature public
API must not grow a single trading type.* Everything here is behind
`execution*` features that are off by default. There is no CI check asserting
that today — it is worth adding, and until it exists the rule is a review
obligation.

## Tests

Tier 1 only — there is no service to stand up.

```bash
# unit tests: the fixed-point helpers, the fold's edge cases, the matcher
cargo test -p wingfoil --features execution-sim --lib adapters::execution

# the P0 gate: the loop closes, with exact values and tick times
cargo test -p wingfoil --features execution-sim --test execution_loop

# the doc fences in mod.rs
cargo test -p wingfoil --features execution-sim --doc adapters::execution
```

`tests/execution_loop.rs` is `#![cfg(feature = "execution-sim")]` and carries
the **named reference strategy**, `ReferenceQuoter`. That strategy is a *test
instrument*: gate P2's exit criterion is that re-running it unchanged against an
honest fill model gives materially worse PnL for named reasons (queue position,
fees, latency), so do not tune it and do not delete it.

CI runs both through `rust-test.yml`'s `test` job, which is all-features.

## Example

`examples/adapters/execution/` (target `execution_adapter`,
`required-features = ["execution-sim"]`) — the loop with the same strategy,
printing as it goes. It runs under `HistoricalFrom`, so the output pinned in
its README is reproducible; keep it that way if you change the feed.

## Python bindings

None yet. Gate **P3** binds the types and the fold (not the simulator — the
types are what a Python strategy needs first), via `/bind-adapter`.
