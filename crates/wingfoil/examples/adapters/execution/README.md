# Execution Adapter Examples (wingfoil)

The venue- and asset-neutral execution layer
([`adapters::execution`](../../../src/adapters/execution/mod.rs)), offline and
instant. Every one runs on instruments that are not the ones it was first
built for — equity tickers, an index future, a variance swap — to show nothing
in it knows what it trades.

| Example | Run | What it shows |
|---|---|---|
| [`order_edge`](order_edge/) | `--example execution_order_edge` | `Order` constructors and `validate`, the inert default, epoch-carrying ids, `Request` / `Report`, and `ExecId` refusing what it cannot hold. |
| [`oms_quote`](oms_quote/) | `--example execution_oms_quote` | Quoting two equities through `Oms::diff`: one request in flight per slot, amends on a moved touch, withdrawal by decision and by age, and the order-rate budget deferring and re-planning. |
| [`position_book`](position_book/) | `--example execution_position_book` | A `Book` valued by the caller's `Measure`: linear equity PnL, an index future at $50 a point, an inverse future's convexity, a cashflow, an unvalued variance swap, and totals one currency at a time. |
| [`reconcile`](reconcile/) | `--example execution_reconcile` | A position `Fold` read against the venue's holdings: silent before the venue speaks, a grace, and a re-base at the fold's mark. |
| [`latch_and_cap`](latch_and_cap/) | `--example execution_latch_and_cap` | The kill switch over limits of your own — latch, `clear` refusing, `NaN` outside the cap, a restart — and the per-order `Cap` judged by `Judge`. |
| [`message_ratio`](message_ratio/) | `--example execution_message_ratio` | A venue's message-to-trade `Ratio`: amends held once it is used up, a fill buying more, and a withdrawal never held. |
| [`replace_chain`](replace_chain/) | `--example execution_replace_chain` | The unchanged OMS in front of a FIX-shaped venue through `ReplaceChain`: a replace renaming the order at the venue but not in the OMS, a fill on the renamed order, and cancel-all as one cancel per order. |
| [`exchange_day`](exchange_day/) | `--example execution_exchange_day` | A trading day at an exchange with no post-only: day limits, a halt holding a re-price but not a withdrawal, the close expiring what rests, the next open re-quoting, and a limit priced through the touch trading as a taker. |

## Prerequisites

None. All need the `execution` feature; `replace_chain` and `exchange_day`
need `execution-testing` for `testing::FixVenue`.

## Run

```sh
cargo run -p wingfoil --example execution_oms_quote --features execution
cargo run -p wingfoil --example execution_exchange_day --features execution-testing
```
