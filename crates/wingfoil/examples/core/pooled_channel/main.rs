//! Loan-based ingress with recycled payload buffers and bounded backpressure.
//!
//! A producer thread loans a pre-sized [`Book`], writes into its existing
//! vectors, and sends the unique owner into the graph. The graph receives a
//! [`Pooled<Book>`](wingfoil::pool::Pooled): cloning that handle only bumps an
//! `Rc`, so routing and enrichment never deep-clone the book. When the last
//! graph-side handle drops, the buffer returns to the producer's pool.
//!
//! ```sh
//! cargo run -p wingfoil --example pooled_channel
//! ```

use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::thread;
use std::time::{Duration, Instant};

use anyhow::{Context, ensure};
use wingfoil::pool::Pooled;
use wingfoil::prelude::*;
use wingfoil::{RunFor, RunMode};

const POOL_CAPACITY: usize = 2;
const BOOK_DEPTH: usize = 4;

#[derive(Debug)]
struct Book {
    seq: u64,
    bids: Vec<(f64, u64)>,
    asks: Vec<(f64, u64)>,
}

impl Book {
    fn with_depth() -> Self {
        Self {
            seq: 0,
            bids: Vec::with_capacity(BOOK_DEPTH),
            asks: Vec::with_capacity(BOOK_DEPTH),
        }
    }

    /// Overwrite one recycled book without replacing either vector's heap
    /// allocation. A real decoder would fill the same way.
    fn refill(&mut self, seq: u64) {
        self.seq = seq;
        self.bids.clear();
        self.asks.clear();

        let mid = 100.0 + seq as f64;
        for level in 0..BOOK_DEPTH {
            let offset = 0.05 + level as f64 * 0.01;
            self.bids.push((mid - offset, 10 + level as u64));
            self.asks.push((mid + offset, 12 + level as u64));
        }
    }

    fn spread(&self) -> f64 {
        let bid = self
            .bids
            .first()
            .expect("invariant: refill writes at least one bid")
            .0;
        let ask = self
            .asks
            .first()
            .expect("invariant: refill writes at least one ask")
            .0;
        ask - bid
    }
}

/// Enrichment wraps the pooled handle instead of cloning the heap-owning
/// payload. `Book` deliberately has no `Clone` implementation.
#[derive(Clone, Default)]
struct EnrichedBook {
    book: Pooled<Book>,
    spread: f64,
}

fn wait_for_book(phase: &AtomicU64, seq: u64) -> anyhow::Result<()> {
    let deadline = Instant::now() + Duration::from_secs(5);
    while phase.load(Ordering::Acquire) < seq {
        if Instant::now() >= deadline {
            anyhow::bail!("timed out waiting for graph to print book #{seq}");
        }
        thread::sleep(Duration::from_millis(1));
    }
    Ok(())
}

fn main() -> anyhow::Result<()> {
    let g = GraphBuilder::new();
    let phase = Arc::new(AtomicU64::new(0));
    let (books, mut sender) = g.pooled_channel_with(POOL_CAPACITY, Book::with_depth);

    let printed_phase = Arc::clone(&phase);
    let _printed = books
        // Take the newest item of each arrival burst. `collapse` iterates the
        // burst by reference and clones only what it emits — here the
        // graph-side Rc handle, never Book or either Vec. Every other item in
        // the burst is dropped, which is the right trade for a
        // latest-book-wins signal and wrong for an event stream; see
        // `StreamOps::collapse`.
        .collapse::<Pooled<Book>>()
        // Wrap-don't-clone: retain the handle and add derived information.
        .map(|book: &Pooled<Book>| EnrichedBook {
            spread: book.spread(),
            book: book.clone(),
        })
        .for_each(move |event: &EnrichedBook| {
            let best_bid = event
                .book
                .bids
                .first()
                .expect("invariant: refill writes at least one bid")
                .0;
            let best_ask = event
                .book
                .asks
                .first()
                .expect("invariant: refill writes at least one ask")
                .0;
            println!(
                "graph: book #{} best {best_bid:.2} x {best_ask:.2}, spread {:.2}",
                event.book.seq, event.spread
            );
            printed_phase.store(event.book.seq, Ordering::Release);
            Ok(())
        });

    let producer = thread::spawn(move || {
        let result = (|| -> anyhow::Result<()> {
            println!("pool: {POOL_CAPACITY} pre-sized books, {BOOK_DEPTH} levels per side");

            let mut first = sender.loan();
            let first_buffer = (&*first as *const Book) as usize;
            first.refill(1);
            ensure!(sender.send(first), "graph receiver closed before book #1");
            wait_for_book(&phase, 1)?;

            // Book #1 is now retained by the routing/enrichment value slots,
            // leaving one of the two pool buffers available to the producer.
            let available = sender.available();
            ensure!(
                available == 1,
                "expected one free buffer while the graph holds book #1, got {available}"
            );
            println!(
                "backpressure: graph holds book #1; {available}/{POOL_CAPACITY} buffers remain available"
            );

            let mut second = sender
                .try_loan()
                .context("the second pool buffer should still be available")?;
            ensure!(
                sender.try_loan().is_none(),
                "try_loan must fail once the bounded loan budget is exhausted"
            );
            println!("backpressure: loaning the last free buffer makes try_loan() return None");

            second.refill(2);
            ensure!(sender.send(second), "graph receiver closed before book #2");
            wait_for_book(&phase, 2)?;

            // Updating the graph's slots to book #2 dropped the last handle to
            // book #1. Its original buffer is now back in the producer pool.
            let mut recycled = sender.loan();
            ensure!(
                (&*recycled as *const Book) as usize == first_buffer,
                "expected book #1's buffer to be the next recycled loan"
            );
            ensure!(
                recycled.bids.capacity() >= BOOK_DEPTH && recycled.asks.capacity() >= BOOK_DEPTH,
                "recycled Vec capacity must survive clear-and-refill"
            );
            println!("recycling: book #1's buffer returned with its Vec capacity intact");

            recycled.refill(3);
            ensure!(
                sender.send(recycled),
                "graph receiver closed before book #3"
            );
            sender.close();
            Ok(())
        })();

        if let Err(error) = &result {
            sender.send_error(anyhow::anyhow!("producer failed: {error:#}"));
        }
        result
    });

    let mut runner = g.build();
    runner.run(RunMode::RealTime, RunFor::Forever)?;
    producer
        .join()
        .map_err(|_| anyhow::anyhow!("producer thread panicked"))??;
    Ok(())
}
