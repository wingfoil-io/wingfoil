# Execution OMS Quote Example (wingfoil)

Quoting two equities through `Oms::diff`: one request in flight per slot, amends on a moved touch, withdrawal by decision and by age, and the order-rate budget deferring and re-planning.

## Prerequisites

None — offline and instant.

## Run

```sh
cargo run -p wingfoil --example execution_oms_quote --features execution
```

## Code

A market maker states what it wants showing each tick as a `Desired`; the OMS emits only the `Request`s that close the gap to what is working, and acks fed back as `Report`s move each slot on. See [`main.rs`](main.rs).

## Output

```text
t=0 two two-ways:
  place  1 Aapl Bid 100 @ 189.5
  place  2 Aapl Ask 100 @ 189.6
  place  3 Msft Bid 50 @ 420.9
  place  4 Msft Ask 50 @ 421.1
t=1 same again, nothing acked yet:
  (nothing)
t=300 AAPL re-priced:
  amend  1 → 100 @ 189.55
  amend  2 → 100 @ 189.65
t=600 MSFT withdrawn:
  cancel 3
  cancel 4
t=6000 AAPL gone stale:
  cancel 1
  cancel 2
pacing: Pacing { deferred: 0, superseded: 0, waiting: 0 }
t=10000 three two-ways against a burst of four:
  place  1 Aapl Bid 100 @ 189.5
  place  2 Aapl Ask 100 @ 189.6
  place  3 Msft Bid 50 @ 420.9
  place  4 Msft Ask 50 @ 421.1
pacing: Pacing { deferred: 2, superseded: 0, waiting: 2 }
t=10250 the budget has refilled:
  place  5 Nvda Bid 200 @ 120
  place  6 Nvda Ask 200 @ 120.05
```
