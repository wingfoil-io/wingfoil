//! Island parity for a `collapse` + `fold` interior.
//!
//! [`nested_islands`](nested_islands.rs) covers islands over scalar inputs.
//! This one covers two things that file does not exercise:
//!
//! * a `Stream<Burst<T>>` **input** crossing the island boundary, and
//! * `collapse` as the first op inside the interior — which inside a `nitro!`
//!   block must be spelled without its turbofish, since the forwarder already
//!   takes a `PhantomData` and resolves the element type by inference.
//!
//! As in `nested_islands.rs`, the island is cross-checked against the
//! identical wiring done flat in the same graph, so both run under the same
//! kernel on the same cycles and must agree exactly.
//!
//! This began as a pin for the `trading_e2e` FIX gateway's top-of-book builder,
//! which was briefly mounted as exactly this island. It no longer is: folding
//! the whole burst (rather than collapsing to its last message, which lost
//! market-data updates) leaves a single `fold`, and an island around one node
//! costs more than it saves. The coverage is still worth keeping on its own
//! terms — and the third case below is why that example changed.

use std::time::Duration;

use wingfoil::prelude::*;
use wingfoil::{NanoTime, RunFor, RunMode};

const HISTORICAL: RunMode = RunMode::HistoricalFrom(NanoTime::ZERO);

/// A market-data-ish message: a side tag plus a price.
#[derive(Debug, Clone, Default, PartialEq)]
struct Quote {
    side: u8,
    px: i64,
}

/// The folded top-of-book. Small and `Copy`, which is what makes it a fair
/// `Fold` accumulator — `Fold`'s output *is* its accumulator, cloned once per
/// tick.
#[derive(Debug, Clone, Copy, Default, PartialEq)]
struct Book {
    bid: i64,
    ask: i64,
}

/// The fold body, spelled once here and once inside the `nitro!` block below.
///
/// It cannot be shared as a plain `fn`: `nitro!` takes the call-site argument
/// tokens as the op's `Cfg`, so a named function path becomes part of the
/// config type (`Stream<(Book, fn(&mut Book, &Quote))>`) instead of being
/// called. Combinator arguments inside a `nitro!` block must be literal
/// closures.
macro_rules! fold_quote {
    () => {
        |book: &mut Book, q: &Quote| match q.side {
            0 => book.bid = q.px,
            1 => book.ask = q.px,
            _ => {}
        }
    };
}

wingfoil::nitro! {
    fn top_of_book(g: &GraphBuilder, data: &Stream<Burst<Quote>>) -> Stream<Book> {
        let book = data.collapse().fold(
            Book::default(),
            |book: &mut Book, q: &Quote| match q.side {
                0 => book.bid = q.px,
                1 => book.ask = q.px,
                _ => {}
            },
        );
        book
    }
}

/// One quote per burst, alternating sides, so both fold arms are exercised
/// across the run and the accumulator visibly carries state between cycles.
fn alternating(g: &GraphBuilder) -> Stream<Burst<Quote>> {
    g.ticker(Duration::from_nanos(10)).count().map(|c: &u64| {
        let n = *c as i64;
        burst![Quote {
            side: (n % 2) as u8,
            px: n
        }]
    })
}

/// Two quotes per burst — the case that pins `collapse`'s contract.
fn paired(g: &GraphBuilder) -> Stream<Burst<Quote>> {
    g.ticker(Duration::from_nanos(10)).count().map(|c: &u64| {
        let n = *c as i64;
        burst![Quote { side: 0, px: n }, Quote { side: 1, px: n + 1 }]
    })
}

#[test]
fn collapse_fold_island_matches_flat_wiring() {
    let g = GraphBuilder::new();
    let src = alternating(&g);

    let island = top_of_book::nested(&g, &src).accumulate();
    let flat = src
        .collapse::<Quote>()
        .fold(Book::default(), fold_quote!())
        .accumulate();

    let mut r = g.build();
    r.run(HISTORICAL, RunFor::Cycles(5)).unwrap();

    // `count()` starts at 1, so the sides run 1, 0, 1, 0, 1 and each side
    // holds its last price while the other updates.
    let flat_values = r.value(&flat);
    assert_eq!(
        vec![
            Book { bid: 0, ask: 1 },
            Book { bid: 2, ask: 1 },
            Book { bid: 2, ask: 3 },
            Book { bid: 4, ask: 3 },
            Book { bid: 4, ask: 5 },
        ],
        flat_values,
    );
    assert_eq!(flat_values, r.value(&island));
}

#[test]
fn collapse_fold_island_matches_flat_tick_times() {
    let g = GraphBuilder::new();
    let src = alternating(&g);

    let island = top_of_book::nested(&g, &src).with_time().accumulate();
    let flat = src
        .collapse::<Quote>()
        .fold(Book::default(), fold_quote!())
        .with_time()
        .accumulate();

    let mut r = g.build();
    r.run(HISTORICAL, RunFor::Cycles(5)).unwrap();

    // Values *and* tick times: an island that produced the right numbers on
    // the wrong cycles would still be a regression.
    assert_eq!(r.value(&flat), r.value(&island));
}

/// `collapse` ticks the **last** item of the iterable, not each one — so a
/// multi-item burst folds once per cycle and the earlier items are dropped.
/// Here that means `bid` is never written at all, because the side-0 quote is
/// always followed by a side-1 quote in the same burst.
///
/// This is the documented contract (and what legacy's `collapse` did), not an
/// island artefact — which is the point of asserting it on both paths. Any
/// `fold` fed by `collapse` inherits it, including the FIX gateway's
/// top-of-book builder.
#[test]
fn collapse_keeps_only_the_last_burst_item_on_both_paths() {
    let g = GraphBuilder::new();
    let src = paired(&g);

    let island = top_of_book::nested(&g, &src).accumulate();
    let flat = src
        .collapse::<Quote>()
        .fold(Book::default(), fold_quote!())
        .accumulate();

    let mut r = g.build();
    r.run(HISTORICAL, RunFor::Cycles(3)).unwrap();

    let flat_values = r.value(&flat);
    assert_eq!(
        vec![
            Book { bid: 0, ask: 2 },
            Book { bid: 0, ask: 3 },
            Book { bid: 0, ask: 4 },
        ],
        flat_values,
    );
    assert_eq!(flat_values, r.value(&island));
}
