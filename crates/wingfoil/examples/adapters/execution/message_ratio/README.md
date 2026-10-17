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
0ns sent place 1 Bund Bid 10 @ 131 (PostOnly, GoodTillCancel)
0ns wants bid 131      messages 1 of 4, fills 0
2ms heard ack 1 as 1
3ms sent amend 1 Bund → 10 @ 131.01
3ms wants bid 131.01   messages 2 of 4, fills 0
5ms heard ack 1 as 2
6ms sent amend 1 Bund → 10 @ 131.02
6ms wants bid 131.02   messages 3 of 4, fills 0
8ms heard ack 1 as 3
9ms sent amend 1 Bund → 10 @ 131.03
9ms wants bid 131.03   messages 4 of 4, fills 0
11ms heard ack 1 as 4
12ms wants bid 131.04   messages 4 of 4, fills 0 — held
15ms wants bid 131.05   messages 4 of 4, fills 0 — held
21ms heard fill 1 Bund Bid 10 @ 131.03 (Maker), 0 left
21ms sent place 2 Bund Bid 10 @ 131.05 (PostOnly, GoodTillCancel)
23ms heard ack 2 as 5
24ms sent amend 2 Bund → 10 @ 131.06
24ms wants bid 131.06   messages 6 of 7, fills 1
26ms heard ack 2 as 6
27ms sent amend 2 Bund → 10 @ 131.07
27ms wants bid 131.07   messages 7 of 7, fills 1
29ms heard ack 2 as 7
30ms wants bid 131.08   messages 7 of 7, fills 1 — held
33ms sent cancel 2
33ms wants nothing      messages 8 of 7, fills 1
35ms heard cancelled 2, 10 left
```
