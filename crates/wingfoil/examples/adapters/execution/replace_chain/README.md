# Execution Replace Chain Example (wingfoil)

The unchanged OMS in front of a FIX-shaped venue through `ReplaceChain`: a replace renaming the order at the venue but not in the OMS, a fill on the renamed order, and cancel-all as one cancel per order.

## Prerequisites

None — offline and instant. `testing::FixVenue` is behind the `execution-testing` feature.

## Run

```sh
cargo run -p wingfoil --example execution_replace_chain --features execution-testing
```

## Code

`fix::ReplaceChain` maps the OMS's one `ClientOrderId` per order to the venue's current `ClOrdID`, so the diff never learns a venue id exists. `testing::FixVenue` is the venue. See [`main.rs`](main.rs).

## Output

```text
quote 4999.75 / 5000.50:
  → New     ClOrdID  1  Bid 5 @ 4999.75
  → New     ClOrdID  2  Ask 5 @ 5000.5
  ← New                                on ClOrdID  1  ⇒  OMS order 1
  ← New                                on ClOrdID  2  ⇒  OMS order 2

re-price to 4999.50 / 5000.75 — the venue renames both orders:
  → Replace ClOrdID  3  OrigClOrdID 1  → 5 @ 4999.5
  → Replace ClOrdID  4  OrigClOrdID 2  → 5 @ 5000.75
  ← Replaced → 5 @ 4999.5              on ClOrdID  3  ⇒  OMS order 1
  ← Replaced → 5 @ 5000.75             on ClOrdID  4  ⇒  OMS order 2
  OMS bid is still order 1; at the venue it went ClOrdID 1 → 3

the market trades down through the bid — a fill on the renamed order:
  ← Trade 5 @ 4999.5 (Maker), 0 left   on ClOrdID  3  ⇒  OMS order 1

cancel-all, on a venue with no mass cancel — one cancel per live order:
  → Cancel  ClOrdID  5  OrigClOrdID 4
  ← Canceled, 5 left                   on ClOrdID  5  ⇒  OMS order 2
  nothing resting, nothing live in the chain, unrouted reports: 0
```
