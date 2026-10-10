# Execution Exchange Day Example (wingfoil)

A trading day at an exchange with no post-only: day limits, a halt holding a re-price but not a withdrawal, the close expiring what rests, the next open re-quoting, and a limit priced through the touch trading as a taker.

## Prerequisites

None — offline and instant. `testing::FixVenue` is behind the `execution-testing` feature.

## Run

```sh
cargo run -p wingfoil --example execution_exchange_day --features execution-testing
```

## Code

`Config::passive = Limit` and `Config::lifetime = Day` rest quotes as day limits; the venue's `TradingState`, fed to `Oms::trading`, holds all but cancels while the book is not open. See [`main.rs`](main.rs).

## Output

```text
09:31 quote both: sent ["place", "place", "place", "place"]; resting at the venue: 4
  resting as Limit, Day
[Halted] 0 order(s) expired
10:01 halted: re-price AAPL, pull MSFT: sent ["cancel", "cancel"]; resting at the venue: 2
[Open] 0 order(s) expired
10:05 reopened: the held re-price goes: sent ["amend", "amend"]; resting at the venue: 2
[Closed] 2 order(s) expired
16:01 closed: still wanted: sent []; resting at the venue: 0
[Open] 0 order(s) expired
next 09:30 open: sent ["place", "place"]; resting at the venue: 2
MSFT bid priced through the ask: 2 placed, 1 filled at once as a taker, 1 rest
```
