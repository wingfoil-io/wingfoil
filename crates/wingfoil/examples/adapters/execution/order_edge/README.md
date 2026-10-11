# Execution Order Edge Example (wingfoil)

`Order` constructors and `validate`, the inert default, epoch-carrying ids, `Request` / `Report`, and `ExecId` refusing what it cannot hold.

## Prerequisites

None — offline and instant.

## Run

```sh
cargo run -p wingfoil --example execution_order_edge --features execution
```

## Code

The vocabulary itself, with no graph: an equity ticker enum as the instrument, the three order constructors, `Order::validate` on orders that say two things, and a `Request` / `Report` round trip. See [`main.rs`](main.rs).

This is the one execution example that deliberately wires no graph. Its subject is the vocabulary — what an `Order` can and cannot say — rather than behaviour at instants, so there is nothing a graph would show that a sequence of constructor calls does not. Every other example in the group runs a graph.

## Output

```text
id 1 of epoch 7 is 30064771073 (epoch 7, seq 1)
Aapl Bid 100 PostOnly @ 189.5 GoodTillCancel → Ok(())
Msft Bid 50 Limit @ 421 GoodTillCancel → Ok(())
Msft Ask 50 Market @ market ImmediateOrCancel → Ok(())
a post-only IOC and a defaulted order are both refused
request place → order Some(1)
request amend → order Some(1)
request cancel → order Some(1)
request cancel-all → order None
report about Some(ClientOrderId(30064771073)) at 1000000000
report about Some(ClientOrderId(30064771073)) at 1000000000
report about Some(ClientOrderId(30064771074)) at 1000000000
a 200-character exec id is refused
```
