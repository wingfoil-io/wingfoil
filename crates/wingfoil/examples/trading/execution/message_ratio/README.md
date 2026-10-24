# Execution Message Ratio Example (wingfoil)

A venue's message-to-trade `Ratio`, spent by the OMS node in a graph: amends held once it is used up, a fill buying more, and a withdrawal never held.

## Prerequisites

None — offline and instant. `testing::SimVenue` is behind the `execution-testing` feature.

## Run

```sh
cargo run -p wingfoil --example execution_message_ratio --features execution-testing
```

## Code

`Config::ratio` makes the OMS spend the venue's ratio alongside its per-second limits. The quoter's decisions are replayed into the OMS node, opened on the test venue (`OmsLoopOps`); a `reading` of the OMS after each diff prints where the count stands, and the one fill is the market trading down through the resting bid at the venue. A cancel is counted but never held — a kill switch must always be able to pull its quotes — which is why the last count runs over the room. See [`main.rs`](main.rs).

## Output

```text
0.000_000_000 wants bid 131      messages 1 of 4, fills 0
0.000_000_000 sent place 1 Bund Bid 10 @ 131 (PostOnly, GoodTillCancel)
0.002_000_001 heard ack 1 venue 1
0.003_000_000 wants bid 131.01   messages 2 of 4, fills 0
0.003_000_000 sent amend 1 Bund → 10 @ 131.01
0.005_000_001 heard ack 1 venue 2
0.006_000_000 wants bid 131.02   messages 3 of 4, fills 0
0.006_000_000 sent amend 1 Bund → 10 @ 131.02
0.008_000_001 heard ack 1 venue 3
0.009_000_000 wants bid 131.03   messages 4 of 4, fills 0
0.009_000_000 sent amend 1 Bund → 10 @ 131.03
0.011_000_001 heard ack 1 venue 4
0.012_000_000 wants bid 131.04   messages 4 of 4, fills 0 — held
0.015_000_000 wants bid 131.05   messages 4 of 4, fills 0 — held
0.021_000_000 heard fill 1 Bund Bid 10 @ 131.03 (Maker), 0 left, exec T1
0.021_000_000 sent place 2 Bund Bid 10 @ 131.05 (PostOnly, GoodTillCancel)
0.023_000_001 heard ack 2 venue 5
0.024_000_000 wants bid 131.06   messages 6 of 7, fills 1
0.024_000_000 sent amend 2 Bund → 10 @ 131.06
0.026_000_001 heard ack 2 venue 6
0.027_000_000 wants bid 131.07   messages 7 of 7, fills 1
0.027_000_000 sent amend 2 Bund → 10 @ 131.07
0.029_000_001 heard ack 2 venue 7
0.030_000_000 wants bid 131.08   messages 7 of 7, fills 1 — held
0.033_000_000 wants nothing      messages 8 of 7, fills 1
0.033_000_000 sent cancel 2
0.035_000_001 heard cancelled 2, 10 left
```
