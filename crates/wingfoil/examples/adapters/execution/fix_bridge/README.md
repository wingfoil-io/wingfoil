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
0.001_000_000 sent place 1 Es Bid 5 @ 4999.75 (PostOnly, GoodTillCancel)
0.001_000_000 sent place 2 Es Ask 5 @ 5000.5 (PostOnly, GoodTillCancel)
0.003_000_001 heard ack 1 venue 1
0.003_000_001 heard ack 2 venue 2
0.004_000_000 sent amend 1 Es → 5 @ 4999.5
0.004_000_000 sent amend 2 Es → 5 @ 5000.75
0.006_000_001 heard ack 1 venue 3
0.006_000_001 heard ack 2 venue 4
0.007_000_000 heard fill 1 Es Bid 5 @ 4999.5 (Maker), 0 left
0.007_000_000 sent place 3 Es Bid 5 @ 4999.5 (PostOnly, GoodTillCancel)
0.009_000_001 heard ack 3 venue 5
0.010_000_000 sent cancel all
0.012_000_001 heard cancelled 2, 5 left
0.012_000_001 heard cancelled 3, 5 left
```
