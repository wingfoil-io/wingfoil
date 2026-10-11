# Execution Reconcile Example (wingfoil)

A position `Fold` read against the venue's holdings, as a node in a graph: silent before the venue speaks, a grace judged on a sweep, and a re-base at the fold's mark.

## Prerequisites

None — offline and instant.

## Run

```sh
cargo run -p wingfoil --example execution_reconcile --features execution
```

## Code

A `Reconciler` compares the fold with each `Holdings` the venue sends; a difference that outlasts `Config::grace` re-bases the fold on the venue and is reported as a `Mismatch` for the caller to latch its risk on. Both are node state in the library's reconciliation node, wired as `fills.wire_reconcile(config, measure, &holdings).marks(&marks).sweep(&sweep).build()`: fills first (folded — an ordinary fill is not a settlement), then the venue's snapshots in order, then the marks a re-base is booked at, then `reconcile` at the engine time on every cycle — the one-second sweep's included, which is what judges the grace with nothing new arriving. It emits the mismatches, ticking only when there are any, and a `Standing` every cycle. See [`main.rs`](main.rs).

## Output

```text
t=0 venue silent: nothing judged
t=2 1 position(s) disagree, inside the grace
t=7 mismatch on Msft: ours 0, venue 30, since t=2 → re-based
     Msft now net 30, entry 420 — the book's own mark
t=7 agreed; re-based 1 position(s) in all
```
