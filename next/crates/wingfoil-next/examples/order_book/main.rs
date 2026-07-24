#![doc = include_str!("./README.md")]
//!
//! ```sh
//! cargo run -p wingfoil-next --example order_book
//! ```

use std::collections::BTreeMap;
use std::time::Duration;

use wingfoil::{NanoTime, RunFor, RunMode};
use wingfoil_next::prelude::*;

/// A limit order book: resting bid/ask quantity keyed by price. Kept in `fold`
/// state and updated one synthetic message at a time.
#[derive(Clone)]
struct Book {
    rng: u64,
    bids: BTreeMap<u64, u64>,
    asks: BTreeMap<u64, u64>,
}

impl Book {
    fn new(seed: u64) -> Self {
        Book {
            rng: seed,
            bids: BTreeMap::new(),
            asks: BTreeMap::new(),
        }
    }

    /// Deterministic LCG — a self-contained synthetic order feed so the example
    /// needs no data file.
    fn next_rand(&mut self) -> u64 {
        self.rng = self
            .rng
            .wrapping_mul(6_364_136_223_846_793_005)
            .wrapping_add(1_442_695_040_888_963_407);
        self.rng >> 33
    }

    /// Apply one synthetic message: mostly resting limit orders around a mid of
    /// 100, occasionally an aggressive order that crosses and consumes a level.
    fn apply(&mut self) {
        let r = self.next_rand();
        let is_bid = r & 1 == 0;
        let offset = (r >> 1) % 5; // 0..4 ticks off the mid
        let qty = 1 + (r >> 4) % 9; // 1..9

        if r.is_multiple_of(7) {
            // Aggressive order: take the best level on the opposite side.
            if is_bid {
                if let Some((&px, _)) = self.asks.iter().next() {
                    self.asks.remove(&px);
                }
            } else if let Some((&px, _)) = self.bids.iter().next_back() {
                self.bids.remove(&px);
            }
            return;
        }

        // Resting limit order: bids below the mid, asks above it.
        if is_bid {
            let px = 100 - offset;
            *self.bids.entry(px).or_insert(0) += qty;
        } else {
            let px = 101 + offset;
            *self.asks.entry(px).or_insert(0) += qty;
        }
    }

    fn top(&self) -> TwoWayPrice {
        TwoWayPrice {
            bid: self.bids.keys().next_back().copied(),
            ask: self.asks.keys().next().copied(),
        }
    }
}

/// Top of book: best bid and best ask (either side may be empty).
#[derive(Clone, Default, PartialEq)]
struct TwoWayPrice {
    bid: Option<u64>,
    ask: Option<u64>,
}

fn main() -> anyhow::Result<()> {
    let g = GraphBuilder::new();

    // One synthetic message per tick, folded into the book. `fold` emits the
    // whole book each tick; we then project to top-of-book, keep only the ticks
    // where it *changed*, and print them.
    let book = g
        .ticker(Duration::from_millis(1))
        .fold(Book::new(0x1234_5678), |book, _| book.apply());

    let top = book.map(|b| b.top()).distinct();

    let _out = top.for_each(|p| {
        let fmt = |o: &Option<u64>| o.map_or_else(|| "  -".to_string(), |v| format!("{v:>3}"));
        let spread = match (p.bid, p.ask) {
            (Some(b), Some(a)) => format!("{}", a as i64 - b as i64),
            _ => "-".to_string(),
        };
        println!(
            "bid {}   ask {}   spread {}",
            fmt(&p.bid),
            fmt(&p.ask),
            spread
        );
        Ok(())
    });

    let mut runner = g.build();
    runner.run(RunMode::HistoricalFrom(NanoTime::ZERO), RunFor::Cycles(200))?;
    Ok(())
}
