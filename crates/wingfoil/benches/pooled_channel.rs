//! Payload-strategy comparison on the channel ingress path: the same
//! order-book pipeline (`channel → latest → mid → fold`) three ways —
//!
//! - **owned** — a fresh heap-owning `Book` constructed per send, the naive
//!   baseline (allocates per message; the routing clone is a deep copy);
//! - **arc** — `Arc<Book>` built per send, the pattern a good user writes
//!   today with no engine support (routing clones become atomic refcount
//!   bumps; per-message allocation remains);
//! - **pooled** — `pooled_channel` loans: recycled buffers, in-place refill,
//!   non-atomic graph-side handles (steady-state payload allocations: zero).
//!
//! The producer runs on its own thread sending `MESSAGES` books flat-out
//! (realtime mode, waker-driven, no sleeps), so the measurement covers the
//! steady-state regime the pool exists for — including its backpressure.
//! Throughput is reported per message.
//!
//! `arc` is the honest baseline to beat — see `benches/README.md`.

use std::sync::Arc;
use std::time::{Duration, Instant};

use criterion::{Criterion, Throughput, criterion_group, criterion_main};
use wingfoil::pool::Pooled;
use wingfoil::prelude::*;
use wingfoil::{RunFor, RunMode};

/// One price level: 16 bytes, no heap.
#[derive(Clone, Copy, Default)]
struct Level {
    price: f64,
    qty: f64,
}

/// A fixed-depth order book: ~48B of struct + 2×`DEPTH`×16B of heap.
#[derive(Clone, Default)]
struct Book {
    seq: u64,
    bids: Vec<Level>,
    asks: Vec<Level>,
}

const DEPTH: usize = 128;
const MESSAGES: u64 = 10_000;
const POOL: usize = 64;

impl Book {
    fn with_depth() -> Self {
        Self {
            seq: 0,
            bids: Vec::with_capacity(DEPTH),
            asks: Vec::with_capacity(DEPTH),
        }
    }

    /// Overwrite in place: `clear()` keeps the Vec capacity, so a recycled
    /// book refills without touching the allocator.
    fn refill(&mut self, seq: u64) {
        self.seq = seq;
        self.bids.clear();
        self.asks.clear();
        let base = 100.0 + (seq % 100) as f64 * 0.01;
        for i in 0..DEPTH {
            let d = i as f64 * 0.01;
            self.bids.push(Level {
                price: base - d,
                qty: 1.0 + i as f64,
            });
            self.asks.push(Level {
                price: base + 0.01 + d,
                qty: 1.0 + i as f64,
            });
        }
    }

    /// Touches both fields of both top levels, so the transform reads real
    /// payload data (and no field is dead in the bench).
    fn mid(&self) -> f64 {
        let (b, a) = (&self.bids[0], &self.asks[0]);
        (b.price * a.qty + a.price * b.qty) / (b.qty + a.qty)
    }
}

/// Time one full run: build the graph, spawn the producer, run realtime until
/// the producer closes. Returns elapsed wall time for the whole exchange.
fn timed_run<S, P>(wire: impl FnOnce(&GraphBuilder) -> (Stream<f64>, S), produce: P) -> Duration
where
    S: Send + 'static,
    P: FnOnce(S) + Send + 'static,
{
    let g = GraphBuilder::new();
    let (mids, sender) = wire(&g);
    let sum = mids.fold(0.0f64, |acc, v| *acc += v);
    let mut r = g.build();

    let start = Instant::now();
    let producer = std::thread::spawn(move || produce(sender));
    r.run(RunMode::RealTime, RunFor::Forever)
        .expect("bench run");
    let elapsed = start.elapsed();
    producer.join().expect("producer thread");
    // Keep the fold observable so nothing is optimised away.
    assert!(r.value(&sum) != 0.0);
    elapsed
}

fn bench_payload_strategies(c: &mut Criterion) {
    let mut group = c.benchmark_group("pooled_channel/ingress");
    group.throughput(Throughput::Elements(MESSAGES));
    group.sample_size(10);

    group.bench_function("owned", |b| {
        b.iter_custom(|iters| {
            (0..iters)
                .map(|_| {
                    timed_run(
                        |g| {
                            let (books, sender) = g.channel::<Book>();
                            let mids = books
                                .map(|b: &Burst<Book>| b.last().expect("non-empty burst").clone())
                                .map(|book: &Book| book.mid());
                            (mids, sender)
                        },
                        |sender| {
                            for seq in 0..MESSAGES {
                                let mut book = Book::with_depth();
                                book.refill(seq);
                                sender.send(book);
                            }
                            sender.close();
                        },
                    )
                })
                .sum()
        })
    });

    group.bench_function("arc", |b| {
        b.iter_custom(|iters| {
            (0..iters)
                .map(|_| {
                    timed_run(
                        |g| {
                            let (books, sender) = g.channel::<Arc<Book>>();
                            let mids = books
                                .map(|b: &Burst<Arc<Book>>| {
                                    b.last().expect("non-empty burst").clone()
                                })
                                .map(|book: &Arc<Book>| book.mid());
                            (mids, sender)
                        },
                        |sender| {
                            for seq in 0..MESSAGES {
                                let mut book = Book::with_depth();
                                book.refill(seq);
                                sender.send(Arc::new(book));
                            }
                            sender.close();
                        },
                    )
                })
                .sum()
        })
    });

    group.bench_function("pooled", |b| {
        b.iter_custom(|iters| {
            (0..iters)
                .map(|_| {
                    timed_run(
                        |g| {
                            let (books, sender) = g.pooled_channel_with(POOL, Book::with_depth);
                            let mids = books
                                .map(|b: &Burst<Pooled<Book>>| {
                                    b.last().expect("non-empty burst").clone()
                                })
                                .map(|book: &Pooled<Book>| book.mid());
                            (mids, sender)
                        },
                        |mut sender| {
                            for seq in 0..MESSAGES {
                                let mut loan = sender.loan();
                                loan.refill(seq);
                                sender.send(loan);
                            }
                            sender.close();
                        },
                    )
                })
                .sum()
        })
    });

    group.finish();
}

criterion_group!(benches, bench_payload_strategies);
criterion_main!(benches);
