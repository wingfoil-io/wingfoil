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
0.000_000_000 sent place 1 Aapl Bid 100 @ 189.5 (PostOnly, GoodTillCancel)
0.000_000_000 sent place 2 Aapl Ask 100 @ 189.6 (PostOnly, GoodTillCancel)
0.000_000_000 sent place 3 Msft Bid 50 @ 420.9 (PostOnly, GoodTillCancel)
0.000_000_000 sent place 4 Msft Ask 50 @ 421.1 (PostOnly, GoodTillCancel)
0.000_000_000 pacing Pacing { deferred: 0, deferred_by_budget: 0, deferred_by_ratio: 0, superseded: 0, waiting: 0 }
0.002_000_001 heard ack 1 venue 1
0.002_000_001 heard ack 2 venue 2
0.002_000_001 heard ack 3 venue 3
0.002_000_001 heard ack 4 venue 4
0.300_000_000 sent amend 1 Aapl → 100 @ 189.55
0.300_000_000 sent amend 2 Aapl → 100 @ 189.65
0.302_000_001 heard ack 1 venue 5
0.302_000_001 heard ack 2 venue 6
0.600_000_000 sent cancel 3
0.600_000_000 sent cancel 4
0.602_000_001 heard cancelled 3, 50 left
0.602_000_001 heard cancelled 4, 50 left
5.300_000_001 sent cancel 1
5.300_000_001 sent cancel 2
5.302_000_002 heard cancelled 1, 100 left
5.302_000_002 heard cancelled 2, 100 left
10.000_000_000 sent place 5 Aapl Bid 100 @ 189.5 (PostOnly, GoodTillCancel)
10.000_000_000 sent place 6 Aapl Ask 100 @ 189.6 (PostOnly, GoodTillCancel)
10.000_000_000 sent place 7 Msft Bid 50 @ 420.9 (PostOnly, GoodTillCancel)
10.000_000_000 sent place 8 Msft Ask 50 @ 421.1 (PostOnly, GoodTillCancel)
10.000_000_000 pacing Pacing { deferred: 2, deferred_by_budget: 2, deferred_by_ratio: 0, superseded: 0, waiting: 2 }
10.002_000_001 heard ack 5 venue 11
10.002_000_001 heard ack 6 venue 12
10.002_000_001 heard ack 7 venue 13
10.002_000_001 heard ack 8 venue 14
10.100_000_000 sent place 9 Nvda Bid 200 @ 120 (PostOnly, GoodTillCancel)
10.100_000_000 pacing Pacing { deferred: 2, deferred_by_budget: 2, deferred_by_ratio: 0, superseded: 0, waiting: 1 }
10.102_000_001 heard ack 9 venue 15
10.200_000_000 sent place 10 Nvda Ask 200 @ 120.05 (PostOnly, GoodTillCancel)
10.200_000_000 pacing Pacing { deferred: 2, deferred_by_budget: 2, deferred_by_ratio: 0, superseded: 0, waiting: 0 }
10.202_000_001 heard ack 10 venue 16
15.000_000_001 sent cancel 5
15.000_000_001 sent cancel 6
15.000_000_001 sent cancel 7
15.000_000_001 sent cancel 8
15.000_000_001 pacing Pacing { deferred: 4, deferred_by_budget: 4, deferred_by_ratio: 0, superseded: 0, waiting: 2 }
15.002_000_002 heard cancelled 5, 100 left
15.002_000_002 heard cancelled 6, 100 left
15.002_000_002 heard cancelled 7, 50 left
15.002_000_002 heard cancelled 8, 50 left
15.100_000_001 sent cancel 9
15.100_000_001 pacing Pacing { deferred: 4, deferred_by_budget: 4, deferred_by_ratio: 0, superseded: 0, waiting: 1 }
15.102_000_002 heard cancelled 9, 200 left
15.200_000_001 sent cancel 10
15.200_000_001 pacing Pacing { deferred: 4, deferred_by_budget: 4, deferred_by_ratio: 0, superseded: 0, waiting: 0 }
15.202_000_002 heard cancelled 10, 200 left
```
