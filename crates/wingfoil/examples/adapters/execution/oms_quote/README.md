# Execution OMS Quote Example (wingfoil)

Quoting equities through the OMS node in a graph: one request in flight per slot, amends on a moved touch, withdrawal by decision and by age, and the order-rate budget deferring and re-planning.

## Prerequisites

None — offline and instant. `testing::SimVenue` is behind the `execution-testing` feature.

## Run

```sh
cargo run -p wingfoil --example execution_oms_quote --features execution-testing
```

## Code

The strategy's decisions are replayed at engine instants into `OmsOps` (`wire_oms(..).sweep(..).build()`). What the OMS sends goes down a millisecond of wire and through one `GraphBuilder::feedback` cut to `testing::SimVenue`, whose reports come back up the same wire into the OMS's `reports` input. The sweep is a 250ms ticker: it is what withdraws AAPL once its last decision is older than `max_desired_age`, and what sends the two places the burst of four could not pay for once a token has refilled. Every line is printed at the instant it happened; nothing advances a clock by hand. See [`main.rs`](main.rs).

## Output

```text
t=0ms sent:
  place  1 Aapl Bid 100 @ 189.5
  place  2 Aapl Ask 100 @ 189.6
  place  3 Msft Bid 50 @ 420.9
  place  4 Msft Ask 50 @ 421.1
t=0ms pacing: Pacing { deferred: 0, superseded: 0, waiting: 0 }
t=2ms heard:
  ack       1
  ack       2
  ack       3
  ack       4
t=300ms sent:
  amend  1 → 100 @ 189.55
  amend  2 → 100 @ 189.65
t=302ms heard:
  ack       1
  ack       2
t=600ms sent:
  cancel 3
  cancel 4
t=602ms heard:
  cancelled 3
  cancelled 4
t=5500ms sent:
  cancel 1
  cancel 2
t=5502ms heard:
  cancelled 1
  cancelled 2
t=10000ms sent:
  place  5 Aapl Bid 100 @ 189.5
  place  6 Aapl Ask 100 @ 189.6
  place  7 Msft Bid 50 @ 420.9
  place  8 Msft Ask 50 @ 421.1
t=10000ms pacing: Pacing { deferred: 2, superseded: 0, waiting: 2 }
t=10002ms heard:
  ack       5
  ack       6
  ack       7
  ack       8
t=10250ms sent:
  place  9 Nvda Bid 200 @ 120
  place  10 Nvda Ask 200 @ 120.05
t=10250ms pacing: Pacing { deferred: 2, superseded: 0, waiting: 0 }
t=10252ms heard:
  ack       9
  ack       10
```
