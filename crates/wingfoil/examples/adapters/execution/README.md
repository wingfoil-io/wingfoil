# Execution Adapter Examples (wingfoil)

The venue- and asset-neutral execution layer
([`adapters::execution`](../../../src/adapters/execution/mod.rs)), offline and
instant. Every one runs on instruments that are not the ones it was first
built for — equity tickers, an index future, a variance swap — to show nothing
in it knows what it trades. The map of the layer's types and how they
connect is [`docs/execution-layer.md`](../../../../../docs/execution-layer.md).

| Example | Run | What it shows |
|---|---|---|
| [`strategy_loop`](strategy_loop/) | `--example execution_strategy_loop` | **Start here.** The whole layer in one graph: a quoter, `OmsOps`, `ceiling::Capped`, `testing::SimVenue`, fills folded into a `Book`, the venue's holdings reconciled against it, and a kill switch whose halt is the OMS's cancel-all and the quoter's reason to stop — the loop a venue integration copies. |
| [`order_edge`](order_edge/) | `--example execution_order_edge` | `Order` constructors and `validate`, the inert default, epoch-carrying ids, `Request` / `Report`, and `ExecId` refusing what it cannot hold. |
| [`oms_quote`](oms_quote/) | `--example execution_oms_quote` | Quoting equities through the OMS node in a graph — `OmsOps`, one feedback cut, `testing::SimVenue` answering: one request in flight per slot, amends on a moved touch, withdrawal by decision and by age, and the order-rate budget deferring and re-planning on the sweep. |
| [`position_book`](position_book/) | `--example execution_position_book` | A `Book` valued by the caller's `Measure`, as a node in a graph: linear equity PnL, an index future at $50 a point, an inverse future's convexity, a cashflow, an unvalued variance swap, and totals one currency at a time, each printed at the mark that made it. |
| [`reconcile`](reconcile/) | `--example execution_reconcile` | A position `Fold` read against the venue's holdings, as a node in a graph: silent before the venue speaks, a grace judged on a sweep, and a re-base at the fold's mark. |
| [`latch_and_cap`](latch_and_cap/) | `--example execution_latch_and_cap` | The kill switch over limits of your own, as a node in a graph — latch, `clear` refusing, `NaN` outside the cap, a restart from a `Snapshot` — and the per-order `Cap` judged by `Capped` on the request wire, aborting the run. |
| [`message_ratio`](message_ratio/) | `--example execution_message_ratio` | A venue's message-to-trade `Ratio`, spent by the OMS node in a graph: amends held once it is used up, a fill from the test venue buying more, and a withdrawal never held. |
| [`replace_chain`](replace_chain/) | `--example execution_replace_chain` | The unchanged OMS in front of a FIX-shaped venue, in a graph: a replace renaming the order at the venue (the `VenueId` on each ack) but not in the OMS, a fill on the renamed order, and cancel-all as one cancel per order. |
| [`exchange_day`](exchange_day/) | `--example execution_exchange_day` | A trading day at an exchange with no post-only, in a graph: the venue's `TradingState` replayed through the session, day limits, a halt holding a re-price but not a withdrawal, the close expiring what rests, the next open re-quoting, and a cross taking the offer once. |

## Prerequisites

None. All need the `execution` feature; the five that run a graph against
the test venue — `strategy_loop`, `oms_quote`, `message_ratio`,
`replace_chain` and `exchange_day` — need `execution-testing` for
`testing::SimVenue`.

Every example that shows behaviour wires a graph and runs it under
`RunMode::HistoricalFrom`, printing as the run goes; its output is what that
run printed. `order_edge` is the one exception, on purpose: its subject is
the vocabulary, not behaviour at instants. `position_book`, `reconcile` and
`latch_and_cap` drive the library's position, reconciliation and kill-switch
nodes (`PositionOps`, `ReconcileOps`, `KillSwitchOps`), which the examples'
own ops were the specification for — see
`docs/planning/execution-on-graph-plan.md`.

## Run

```sh
cargo run -p wingfoil --example execution_strategy_loop --features execution-testing
cargo run -p wingfoil --example execution_order_edge --features execution
```
