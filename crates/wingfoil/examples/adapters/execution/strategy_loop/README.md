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
1ms desk net 0 entry - mark 5000.125 pnl 0 — venue silent
1ms switch Open: standing {}, latched {}
1ms sent place 1 Es Bid 1 @ 4999.75 (PostOnly, GoodTillCancel)
1ms sent place 2 Es Ask 1 @ 5000.5 (PostOnly, GoodTillCancel)
3ms heard ack 1 as 1
3ms heard ack 2 as 2
101ms heard fill 1 Es Bid 1 @ 4999.75 (Maker), 0 left
101ms desk net 1 entry 4999.75 mark 4999.625 pnl -6.25 — venue silent
101ms sent place 3 Es Bid 1 @ 4999.25 (PostOnly, GoodTillCancel)
101ms sent amend 2 Es → 1 @ 5000
103ms heard ack 3 as 3
103ms heard ack 2 as 4
300ms desk net 1 entry 4999.75 mark 4999.625 pnl -6.25 — 1 position disagrees, inside the grace
400ms desk mismatch: ours 1, venue 2 since 300ms → re-based
400ms desk net 2 entry 4999.6875 mark 4999.625 pnl -6.25 — agrees with the venue
400ms switch Halted: standing {Mismatch}, latched {Mismatch}
400ms sent cancel all
402ms heard cancelled 3, 1 left
402ms heard cancelled 2, 1 left
421ms desk net 2 entry 4999.6875 mark 5000.125 pnl 43.75 — agrees with the venue
421ms switch Halted: standing {}, latched {Mismatch}
450ms switch Open: standing {}, latched {}
461ms sent place 4 Es Bid 1 @ 4999.75 (PostOnly, GoodTillCancel)
461ms sent place 5 Es Ask 1 @ 5000.5 (PostOnly, GoodTillCancel)
463ms heard ack 4 as 6
463ms heard ack 5 as 7
```
