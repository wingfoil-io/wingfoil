//! The steady-state allocation gate: a counting `#[global_allocator]` wrapped
//! around the pooled-channel order-book pipeline, asserting that the payload
//! path stays off the allocator. This is the enforcement half of the pool's
//! "zero payload allocations at steady state" claim — a regression *test*,
//! deliberately not a Criterion bench (wall-clock is noisy on shared runners;
//! allocation counts are exact).
//!
//! What is asserted, and why these numbers:
//!
//! - **Zero large allocations** (≥ `LARGE` bytes) between run start and run
//!   end: a payload (`Book` holds two 2KB level Vecs) can only be built by
//!   crossing this threshold, so zero large allocs proves every book the
//!   graph saw lived in a recycled pool buffer.
//! - **A small per-message budget** for the residuals the pool deliberately
//!   leaves (documented in `pool.rs`): the handle's `Rc` control block and
//!   the transport queue's node, plus occasional `Burst` spill. The budget is
//!   a pinned ceiling so a regression (a clone sneaking onto the path, a
//!   fresh `Vec` per message) fails loudly.
//!
//! One test per binary: the counters are process-global, so this file must
//! not gain a second concurrent `#[test]`.

use std::alloc::{GlobalAlloc, Layout, System};
use std::sync::atomic::{AtomicU64, Ordering};

use wingfoil::pool::Pooled;
use wingfoil::prelude::*;
use wingfoil::{RunFor, RunMode};

/// Payload-sized threshold: anything this big is payload construction. Sized
/// well above the engine's infrastructure blocks (waker-channel slot blocks,
/// thread bookkeeping, queue growth — all ≤ 8KB in practice, and amortized)
/// and at exactly the book's per-side `Vec` block, so a single payload built
/// or deep-cloned outside the pool trips the gate.
const LARGE: usize = 16_000;

static ALLOCS: AtomicU64 = AtomicU64::new(0);
static LARGE_ALLOCS: AtomicU64 = AtomicU64::new(0);
/// First few large-alloc sizes, for the failure message — attribution beats
/// a bare count when this gate trips.
static LARGE_SIZES: [AtomicU64; 8] = [const { AtomicU64::new(0) }; 8];

fn record_large(size: usize) {
    let n = LARGE_ALLOCS.fetch_add(1, Ordering::Relaxed) as usize;
    if let Some(slot) = LARGE_SIZES.get(n) {
        slot.store(size as u64, Ordering::Relaxed);
    }
}

struct Counting;

// SAFETY: delegates every operation to `System` unchanged; the counters are
// relaxed atomics with no side effects on allocation behaviour.
unsafe impl GlobalAlloc for Counting {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        ALLOCS.fetch_add(1, Ordering::Relaxed);
        if layout.size() >= LARGE {
            record_large(layout.size());
        }
        unsafe { System.alloc(layout) }
    }

    unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
        unsafe { System.dealloc(ptr, layout) }
    }

    unsafe fn realloc(&self, ptr: *mut u8, layout: Layout, new_size: usize) -> *mut u8 {
        ALLOCS.fetch_add(1, Ordering::Relaxed);
        if new_size >= LARGE {
            record_large(new_size);
        }
        unsafe { System.realloc(ptr, layout, new_size) }
    }
}

#[global_allocator]
static COUNTING: Counting = Counting;

#[derive(Clone, Copy, Default)]
struct Level {
    price: f64,
    qty: f64,
}

/// Two 1024-level sides: each `Vec` is 16KB — over `LARGE`, so any fresh
/// construction (or deep clone) is caught by the large-alloc counter.
#[derive(Default)]
struct Book {
    seq: u64,
    bids: Vec<Level>,
    asks: Vec<Level>,
}

const DEPTH: usize = 1024;
const MESSAGES: u64 = 1_000;
const POOL: usize = 64;

/// Residual budget per message: the `Pooled` handle's `Rc` box, the mpsc
/// node, and headroom for `Burst` spill — the documented residuals, nothing
/// more. A deep payload clone or per-message construction blows through this
/// immediately (each would add 3 allocs including two large ones).
const PER_MESSAGE_BUDGET: u64 = 8;
/// Slack for one-off run machinery (thread spawn, run start/stop, waker).
const FIXED_SLACK: u64 = 200;

impl Book {
    fn with_depth() -> Self {
        Self {
            seq: 0,
            bids: Vec::with_capacity(DEPTH),
            asks: Vec::with_capacity(DEPTH),
        }
    }

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
}

#[test]
fn pooled_pipeline_stays_off_the_allocator() {
    // Wire and build BEFORE the measured window: graph construction and pool
    // prefill are the setup phase and allocate freely.
    let g = GraphBuilder::new();
    let (books, mut sender) = g.pooled_channel_with(POOL, Book::with_depth);
    let sum = books
        .map(|b: &Burst<Pooled<Book>>| b.last().expect("non-empty burst").clone())
        .map(|book: &Pooled<Book>| {
            let (b, a) = (&book.bids[0], &book.asks[0]);
            (b.price * a.qty + a.price * b.qty) / (b.qty + a.qty)
        })
        .fold(0.0f64, |acc, v| *acc += v);
    let mut r = g.build();

    let allocs_before = ALLOCS.load(Ordering::Relaxed);
    let large_before = LARGE_ALLOCS.load(Ordering::Relaxed);

    let producer = std::thread::spawn(move || {
        for seq in 0..MESSAGES {
            let mut loan = sender.loan();
            loan.refill(seq);
            sender.send(loan);
        }
        sender.close();
    });
    r.run(RunMode::RealTime, RunFor::Forever).expect("run");
    producer.join().expect("producer thread");

    let allocs = ALLOCS.load(Ordering::Relaxed) - allocs_before;
    let large = LARGE_ALLOCS.load(Ordering::Relaxed) - large_before;

    // The sum is data-dependent on every message; keeps the pipeline honest.
    assert!(r.value(&sum) > 0.0);

    // Always visible under --nocapture, so a captured run documents itself.
    eprintln!(
        "steady state: {allocs} allocations for {MESSAGES} messages \
         ({:.2}/message), {large} of payload scale (≥ {LARGE}B)",
        allocs as f64 / MESSAGES as f64
    );

    let sizes: Vec<u64> = LARGE_SIZES
        .iter()
        .map(|s| s.load(Ordering::Relaxed))
        .filter(|&s| s > 0)
        .collect();
    assert_eq!(
        0, large,
        "payload-sized allocations on the steady-state path — a book was \
         built outside the pool ({large} allocs ≥ {LARGE}B in a {MESSAGES}-message \
         run; first sizes: {sizes:?})"
    );
    let budget = MESSAGES * PER_MESSAGE_BUDGET + FIXED_SLACK;
    assert!(
        allocs <= budget,
        "allocation regression: {allocs} allocs for {MESSAGES} messages \
         (budget {budget} = {PER_MESSAGE_BUDGET}/message + {FIXED_SLACK} slack)"
    );
}
