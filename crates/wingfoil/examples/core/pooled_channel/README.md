## Pooled channel — recycled buffers under backpressure

Loan a pre-sized, heap-owning payload on a producer thread, write into it in
place, and send its unique owner into a Wingfoil graph. The graph sees a
`Pooled<Book>` handle: cloning it through routing and enrichment only bumps a
single-threaded `Rc`, rather than deep-cloning the book's `Vec`s.

The capacity-two pool is also the backpressure budget. Once the graph holds
book #1 and the producer loans the other buffer, `try_loan()` returns `None`.
When the graph advances to book #2, every handle to book #1 drops, returning
that buffer — with its interior vector capacity intact — for book #3.

```rust
let (books, mut sender) =
    g.pooled_channel_with(2, Book::with_depth);

let _printed = books
    .collapse::<Pooled<Book>>() // newest of the burst; clones the handle only
    .map(|book: &Pooled<Book>| EnrichedBook {
        spread: book.spread(),
        book: book.clone(), // handle clone, not Book clone
    })
    .for_each(|event: &EnrichedBook| {
        println!("book #{} spread {:.2}", event.book.seq, event.spread);
        Ok(())
    });

let mut first = sender.loan();      // blocks when both buffers are in flight
first.refill(1);                    // clear + fill preserves Vec capacity
sender.send(first);
let second = sender.try_loan().unwrap(); // borrow the remaining free buffer
assert!(sender.try_loan().is_none());    // the loan budget is now exhausted
drop(second);
```

The full program uses a tiny phase gate so the backpressure and recycling
steps are deterministic while the graph runs in realtime. It prints from a
`for_each` sink as values arrive; it does not park handles in `accumulate()`,
which would consume the loan budget for the life of the run.

### Output

```text
pool: 2 pre-sized books, 4 levels per side
graph: book #1 best 100.95 x 101.05, spread 0.10
backpressure: graph holds book #1; 1/2 buffers remain available
backpressure: loaning the last free buffer makes try_loan() return None
graph: book #2 best 101.95 x 102.05, spread 0.10
recycling: book #1's buffer returned with its Vec capacity intact
graph: book #3 best 102.95 x 103.05, spread 0.10
```

### Run

```sh
cargo run -p wingfoil --example pooled_channel
```

Read the [`pool` module docs](../../../src/pool.rs) for the full loan-budget
ledger, empty pre-first-tick handles, pointer-identity equality, and the two
small residual per-message allocations that remain outside the payload.
