# Execution FIX Bridge Example (wingfoil)

The unchanged OMS in front of a FIX-shaped venue, in a graph: a replace renaming the order at the venue but not in the OMS, a fill on the renamed order, and cancel-all as one cancel per order.

## Prerequisites

None — offline and instant. `testing::SimVenue` is behind the `execution-testing` feature.

## Run

```sh
cargo run -p wingfoil --example execution_fix_bridge --features execution-testing
```

## Code

`fix::FixBridge` maps the OMS's one `ClientOrderId` per order to the venue's current `ClOrdID`, so the diff never learns a venue id exists. `testing::SimVenue` is a `FixVenue` behind that bridge as one node, on the far side of the feedback cut; what comes back up the wire is the OMS's own ids, with the venue's current `ClOrdID` carried on each ack as its `VenueId` — which is where the rename shows. The venue has no mass cancel, so the bridge renders the OMS's one `CancelAll` as a cancel per live order. See [`main.rs`](main.rs).

## Output

```text
t=1ms sent  place  order 1 Bid 5 @ 4999.75
t=1ms sent  place  order 2 Ask 5 @ 5000.5
t=3ms heard ack       order 1  resting as ClOrdID 1
t=3ms heard ack       order 2  resting as ClOrdID 2
t=4ms sent  amend  order 1 → 5 @ 4999.5
t=4ms sent  amend  order 2 → 5 @ 5000.75
t=6ms heard ack       order 1  resting as ClOrdID 3
t=6ms heard ack       order 2  resting as ClOrdID 4
t=7ms heard fill      order 1  5 @ 4999.5 (Maker), 0 left
t=7ms sent  place  order 3 Bid 5 @ 4999.5
t=9ms heard ack       order 3  resting as ClOrdID 5
t=10ms sent  cancel all
t=12ms heard cancelled order 2  5 left
t=12ms heard cancelled order 3  5 left
```
