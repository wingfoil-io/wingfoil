# Execution Strategy Loop Example (wingfoil)

The whole execution layer in one graph: a quoter, the OMS, the ceiling, the venue, the position fold, reconciliation and the kill switch — the loop a venue integration copies.

## Prerequisites

None — offline and instant. `testing::SimVenue` is behind the `execution-testing` feature.

## Run

```sh
cargo run -p wingfoil --example execution_strategy_loop --features execution-testing
```

## Code

A toy quoter rests one lot a tick outside each touch it sees (market data takes the wire too, a millisecond behind the venue). Its desireds go into the OMS node (`OmsOps`); what the OMS sends goes down a millisecond of wire, through the one cut `GraphBuilder::feedback_loop` wires, past `ceiling::Capped` and into `testing::SimVenue`, whose reports come back up the same wire into the OMS's `reports` input.

Fills fold into a `Book` held by the library's reconciliation node (`ReconcileOps`) beside a `Reconciler`, which reads the venue's `Holdings` against it on every sweep — one node, because the reconciler's fold is the book. `SimVenue` states no holdings, so a scripted stream stands in for them — live, `session.holdings` goes here, each snapshot as a burst of one. The venue's `session.settlements` is the node's own input: a settlement is folded into the book and tells the reconciler the venue's side is flat, where an ordinary fill does neither of the latter. The touch's mid is the node's `marks` input, what a re-base is booked at. The example's reading of the node — net, mark, PnL, the cycle's mismatches — is assessed by the library's kill-switch node (`KillSwitchOps`), a `Switch` over two limits: the day's loss, and a mismatch; its `stance()` is the edge everything downstream reads. A halt is both the OMS's `cancel_all` and what the quoter reads passively to want nothing, so closing the loop through the switch adds no second cut; an operator's `clear` reopens, and the next touch re-quotes.

The run: the market trades down through the bid at 100ms and the quoter re-quotes lower; at 300ms the venue says it holds two lots where the fold has one; at 400ms that has outlasted the grace, the fold is re-based on the venue, the switch latches and the quotes are pulled; at 450ms the operator clears, and the touch at 460ms is quoted again. See [`main.rs`](main.rs).

## Output

```text
t=  0ms desk   net 0 entry - mark - pnl 0 — venue silent
t=  0ms switch Open: standing {}, latched {}
t=  1ms sent   place  1 Bid 1 @ 4999.75
t=  1ms sent   place  2 Ask 1 @ 5000.5
t=  3ms heard  ack       1
t=  3ms heard  ack       2
t=101ms heard  fill      1 1 @ 4999.75 (Maker)
t=101ms desk   net 1 entry 4999.75 mark 4999.625 pnl -6.25 — venue silent
t=101ms sent   place  3 Bid 1 @ 4999.25
t=101ms sent   amend  2 → 1 @ 5000
t=103ms heard  ack       3
t=103ms heard  ack       2
t=300ms desk   net 1 entry 4999.75 mark 4999.625 pnl -6.25 — 1 position disagrees, inside the grace
t=400ms desk   mismatch: ours 1, venue 2 since t=300ms → re-based
t=400ms desk   net 2 entry 4999.6875 mark 4999.625 pnl -6.25 — agrees with the venue
t=400ms switch Halted: standing {Mismatch}, latched {Mismatch}
t=400ms sent   cancel all
t=402ms heard  cancelled 3
t=402ms heard  cancelled 2
t=421ms desk   net 2 entry 4999.6875 mark 5000.125 pnl 43.75 — agrees with the venue
t=421ms switch Halted: standing {}, latched {Mismatch}
t=450ms switch Open: standing {}, latched {}
t=461ms sent   place  4 Bid 1 @ 4999.75
t=461ms sent   place  5 Ask 1 @ 5000.5
t=463ms heard  ack       4
t=463ms heard  ack       5
```
