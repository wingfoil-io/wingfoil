# Execution OMS Quote Example (wingfoil)

Quoting equities through the OMS node in a graph: one request in flight per slot, amends on a moved touch, withdrawal by decision and by age, and the order-rate budget deferring and re-planning.

## Prerequisites

None — offline and instant. `testing::SimVenue` is behind the `execution-testing` feature.

## Run

```sh
cargo run -p wingfoil --example execution_oms_quote --features execution-testing
```

## Code

The strategy's decisions are replayed at engine instants into the OMS node, opened on the test venue (`g.oms(..).venue(..).desired(..).build()`, `OmsLoopOps`). What the OMS sends goes down a millisecond of wire to `testing::SimVenue` — the venue hears it one engine instant after the OMS sent it, the one cut in the loop, which the OMS node owns — and its reports come back up the same wire into the OMS. No clock is wired: the OMS node wakes itself at its own deadlines. That is what withdraws AAPL at 5300ms, the instant its last decision is older than `max_desired_age`; what sends the two places the burst of four could not pay for at 10100ms and 10200ms, as each token refills; and, once the strategy falls silent after 10s, what withdraws every quote at 15000ms — four cancels on the budget's burst, the last two a token apart. Every line is printed at the instant it happened; nothing advances a clock by hand. See [`main.rs`](main.rs).

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
t=5300ms sent:
  cancel 1
  cancel 2
t=5302ms heard:
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
t=10100ms sent:
  place  9 Nvda Bid 200 @ 120
t=10100ms pacing: Pacing { deferred: 2, superseded: 0, waiting: 1 }
t=10102ms heard:
  ack       9
t=10200ms sent:
  place  10 Nvda Ask 200 @ 120.05
t=10200ms pacing: Pacing { deferred: 2, superseded: 0, waiting: 0 }
t=10202ms heard:
  ack       10
t=15000ms sent:
  cancel 5
  cancel 6
  cancel 7
  cancel 8
t=15000ms pacing: Pacing { deferred: 4, superseded: 0, waiting: 2 }
t=15002ms heard:
  cancelled 5
  cancelled 6
  cancelled 7
  cancelled 8
t=15100ms sent:
  cancel 9
t=15100ms pacing: Pacing { deferred: 4, superseded: 0, waiting: 1 }
t=15102ms heard:
  cancelled 9
t=15200ms sent:
  cancel 10
t=15200ms pacing: Pacing { deferred: 4, superseded: 0, waiting: 0 }
t=15202ms heard:
  cancelled 10
```
