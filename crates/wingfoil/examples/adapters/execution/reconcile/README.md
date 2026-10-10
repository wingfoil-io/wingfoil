# Execution Reconcile Example (wingfoil)

A position `Fold` read against the venue's holdings: silent before the venue speaks, a grace, and a re-base at the fold's mark.

## Prerequisites

None — offline and instant.

## Run

```sh
cargo run -p wingfoil --example execution_reconcile --features execution
```

## Code

A `Reconciler` compares the fold with each `Holdings` the venue sends; a difference that outlasts `Config::grace` re-bases the fold on the venue and is reported as a `Mismatch` for the caller to latch its risk on. See [`main.rs`](main.rs).

## Output

```text
t=1  venue silent: nothing judged
t=2  MSFT disagrees: 1 inside its grace
t=8  mismatch on Msft: ours 0, venue 30 since 1000002000000000 → re-based
     MSFT entry now Some(Px(420000000000)), the book's own mark
t=9  agreed; re-based 1 position(s) in all
```
