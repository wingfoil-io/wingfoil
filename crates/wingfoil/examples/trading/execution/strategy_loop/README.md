# Execution Strategy Loop Example (wingfoil)

The whole execution layer in one graph: a quoter, the OMS, the ceiling, the venue, the position fold, reconciliation and the kill switch — the loop a venue integration copies.

## Prerequisites

None — offline and instant. `testing::SimVenue` is behind the `execution-testing` feature.

## Run

```sh
cargo run -p wingfoil --example execution_strategy_loop --features execution-testing
```

## Code

A toy quoter rests one lot a tick outside each touch it sees (market data takes the wire too, a millisecond behind the venue). The OMS node is opened on the venue first — `g.oms(..).venue(..)` (`OmsLoopOps`), with `testing::SimVenue` modelling a millisecond of wire each way behind `ceiling::Capped` — which hands the strategy the venue's session before it has decided anything; the quoter's desireds are the OMS's last input, and `build` closes the loop. The venue hears each request one engine instant after the OMS sent it, plus the wire, and its reports come back up the same wire into the OMS.

Fills fold into a `Book` held by the library's reconciliation node (`ReconcileOps`) beside a `Reconciler`, which reads the venue's `Holdings` against it on every cycle and wakes itself when a difference's grace runs out — one node, because the reconciler's fold is the book. `SimVenue` states no holdings, so a scripted stream stands in for them — live, `session.holdings` goes here, each snapshot as a burst of one. The venue's `session.settlements` is the node's own input: a settlement is folded into the book and tells the reconciler the venue's side is flat, where an ordinary fill does neither of the latter. The touch's mid is the node's `marks` input, what a re-base is booked at. The example's reading of the node — net, mark, PnL, the cycle's mismatches — is assessed by the library's kill-switch node (`KillSwitchOps`), a `Switch` over two limits: the day's loss, and a mismatch; its `stance()` is the edge everything downstream reads. A halt is both the OMS's `cancel_all` and what the quoter reads passively to want nothing, so closing the loop through the switch needs no cut of the example's own — the OMS node owns the one cut in the layer; an operator's `clear` reopens, and the next touch re-quotes.

The run: the market trades down through the bid at 100ms and the quoter re-quotes lower; at 300ms the venue says it holds two lots where the fold has one; at 400ms that has outlasted the grace, the fold is re-based on the venue, the switch latches and the quotes are pulled; at 450ms the operator clears, and the touch at 460ms is quoted again. See [`main.rs`](main.rs).

## Output

```text
0.001_000_000 desk net 0 entry - mark 5000.125 pnl 0 — venue silent
0.001_000_000 switch Open: standing {}, latched {}
0.001_000_000 sent place 1 Es Bid 1 @ 4999.75 (PostOnly, GoodTillCancel)
0.001_000_000 sent place 2 Es Ask 1 @ 5000.5 (PostOnly, GoodTillCancel)
0.003_000_001 heard ack 1 venue 1
0.003_000_001 heard ack 2 venue 2
0.101_000_000 heard fill 1 Es Bid 1 @ 4999.75 (Maker), 0 left, exec T1
0.101_000_000 desk net 1 entry 4999.75 mark 4999.625 pnl -6.25 — venue silent
0.101_000_000 sent place 3 Es Bid 1 @ 4999.25 (PostOnly, GoodTillCancel)
0.101_000_000 sent amend 2 Es → 1 @ 5000
0.103_000_001 heard ack 3 venue 3
0.103_000_001 heard ack 2 venue 4
0.300_000_000 desk net 1 entry 4999.75 mark 4999.625 pnl -6.25 — 1 position disagrees, inside the grace
0.400_000_000 desk mismatch: ours 1, venue 2 since 0.300_000_000 → re-based
0.400_000_000 desk net 2 entry 4999.6875 mark 4999.625 pnl -6.25 — agrees with the venue
0.400_000_000 switch Halted: standing {Mismatch}, latched {Mismatch}
0.400_000_000 sent cancel all
0.402_000_001 heard cancelled 3, 1 left
0.402_000_001 heard cancelled 2, 1 left
0.421_000_000 desk net 2 entry 4999.6875 mark 5000.125 pnl 43.75 — agrees with the venue
0.421_000_000 switch Halted: standing {}, latched {Mismatch}
0.450_000_000 switch Open: standing {}, latched {}
0.461_000_000 sent place 4 Es Bid 1 @ 4999.75 (PostOnly, GoodTillCancel)
0.461_000_000 sent place 5 Es Ask 1 @ 5000.5 (PostOnly, GoodTillCancel)
0.463_000_001 heard ack 4 venue 6
0.463_000_001 heard ack 5 venue 7
```
