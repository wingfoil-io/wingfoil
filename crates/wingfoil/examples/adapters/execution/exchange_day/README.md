# Execution Exchange Day Example (wingfoil)

A trading day at an exchange with no post-only, in a graph: day limits, a halt holding a re-price but not a withdrawal, the close expiring what rests, the next open re-quoting, a cross taking the offer once, and the second close expiring the day's quotes again.

## Prerequisites

None — offline and instant. `testing::SimVenue` is behind the `execution-testing` feature.

## Run

```sh
cargo run -p wingfoil --example execution_exchange_day --features execution-testing
```

## Code

`Config::passive = Limit` and `Config::lifetime = Day` rest quotes as day limits. The venue's two days — halt, open, close, open, close — are replayed as a `TradingState` stream into `testing::SimVenue`, whose session states it back to the OMS node's `trading_state` input: while the book is not open, only cancels go out, and the close expires every day order. The last decision is a `Desired::cross`, a limit immediate-or-cancel capped at its price that fills as a taker once — a resting bid priced through the ask would be wanted again the instant it filled. See [`main.rs`](main.rs).

## Output

```text
day 1 09:31 sent place 1 Aapl Bid 100 @ 189.95 (Limit, Day)
day 1 09:31 sent place 2 Aapl Ask 100 @ 190.1 (Limit, Day)
day 1 09:31 sent place 3 Msft Bid 100 @ 420.95 (Limit, Day)
day 1 09:31 sent place 4 Msft Ask 100 @ 421.15 (Limit, Day)
day 1 09:31 heard ack 1 venue 1
day 1 09:31 heard ack 2 venue 2
day 1 09:31 heard ack 3 venue 3
day 1 09:31 heard ack 4 venue 4
day 1 10:00 [Halted]
day 1 10:01 sent cancel 3
day 1 10:01 sent cancel 4
day 1 10:01 heard cancelled 3, 100 left
day 1 10:01 heard cancelled 4, 100 left
day 1 10:05 [Open]
day 1 10:05 sent amend 1 Aapl → 100 @ 189.9
day 1 10:05 sent amend 2 Aapl → 100 @ 190.15
day 1 10:05 heard ack 1 venue 7
day 1 10:05 heard ack 2 venue 8
day 1 16:00 [Closed]
day 1 16:00 heard expired 1, 100 left
day 1 16:00 heard expired 2, 100 left
day 2 09:30 [Open]
day 2 09:30 sent place 5 Aapl Bid 100 @ 189.9 (Limit, Day)
day 2 09:30 sent place 6 Aapl Ask 100 @ 190.15 (Limit, Day)
day 2 09:30 heard ack 5 venue 9
day 2 09:30 heard ack 6 venue 10
day 2 09:31 sent place 7 Msft Bid 100 @ 421.2 (Limit, ImmediateOrCancel)
day 2 09:31 heard ack 7 venue 11
day 2 09:31 heard fill 7 Msft Bid 100 @ 421.1 (Taker), 0 left
day 2 16:00 [Closed]
day 2 16:00 heard expired 5, 100 left
day 2 16:00 heard expired 6, 100 left
```
