# Execution FIX Bridge Example (wingfoil)

The unchanged OMS in front of a FIX-shaped venue, in a graph: a replace renaming the order at the venue but not in the OMS, a fill on the renamed order, and cancel-all as one cancel per order.

## Prerequisites

None — offline and instant. `testing::SimVenue` is behind the `execution-testing` feature.

## Run

```sh
cargo run -p wingfoil --example execution_fix_bridge --features execution-testing
```

## Code

`fix::FixBridge` maps the OMS's one `ClientOrderId` per order to the venue's current `ClOrdID`, so the diff never learns a venue id exists. `testing::SimVenue` is a `FixVenue` behind that bridge as one node, the venue the OMS is opened on; what comes back up the wire is the OMS's own ids, with the venue's current `ClOrdID` carried on each ack as its `VenueId` — which is where the rename shows. The venue has no mass cancel, so the bridge renders the OMS's one `CancelAll` as a cancel per live order. See [`main.rs`](main.rs).

## Output

```text
1ms sent place 1 Es Bid 5 @ 4999.75 (PostOnly, GoodTillCancel)
1ms sent place 2 Es Ask 5 @ 5000.5 (PostOnly, GoodTillCancel)
3ms heard ack 1 as 1
3ms heard ack 2 as 2
4ms sent amend 1 Es → 5 @ 4999.5
4ms sent amend 2 Es → 5 @ 5000.75
6ms heard ack 1 as 3
6ms heard ack 2 as 4
7ms heard fill 1 Es Bid 5 @ 4999.5 (Maker), 0 left
7ms sent place 3 Es Bid 5 @ 4999.5 (PostOnly, GoodTillCancel)
9ms heard ack 3 as 5
10ms sent cancel all
12ms heard cancelled 2, 5 left
12ms heard cancelled 3, 5 left
```
