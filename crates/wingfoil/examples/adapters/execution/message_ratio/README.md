# Execution Message Ratio Example (wingfoil)

A venue's message-to-trade `Ratio`: amends held once it is used up, a fill buying more, and a withdrawal never held.

## Prerequisites

None — offline and instant.

## Run

```sh
cargo run -p wingfoil --example execution_message_ratio --features execution
```

## Code

`Config::ratio` makes the OMS spend the venue's ratio alongside its per-second limits. A cancel is counted but never held — a kill switch must always be able to pull its quotes. See [`main.rs`](main.rs).

## Output

```text
tick 0: sent 1 — (messages, fills) = (1, 0)
tick 1: sent 1 — (messages, fills) = (2, 0)
tick 2: sent 1 — (messages, fills) = (3, 0)
tick 3: sent 1 — (messages, fills) = (4, 0)
tick 4: sent 0 — (messages, fills) = (4, 0)
tick 5: sent 0 — (messages, fills) = (4, 0)
resting at 131.03 while the strategy wants 131.05
after a fill: sent 1 — (5, 1)
out of room, withdrawing: [Cancel(ClientOrderId(1))] — (8, 1)
```
