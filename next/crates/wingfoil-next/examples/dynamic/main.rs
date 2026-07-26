#![doc = include_str!("./README.md")]
//!
//! ```sh
//! cargo run -p wingfoil-next --example dynamic --features dynamic-graph
//! ```

use std::collections::BTreeMap;
use std::time::Duration;

use wingfoil::{NanoTime, RunFor, RunMode};
use wingfoil_next::interp::{Extension, Handle};
use wingfoil_next::prelude::*;

type Instrument = u64;
type Price = u64;
type PriceBook = BTreeMap<Instrument, Price>;

/// The run is bounded by wall-clock time rather than a cycle count: under
/// `run_dynamic`, splicing a sub-graph in or out consumes scheduler cycles, so
/// `RunFor::Cycles(n)` would *not* map cleanly to `n` ticker ticks. A
/// [`RunFor::Duration`] bound is splice-independent — with a 1-second ticker it
/// yields a fixed, reproducible set of ticks (here `n = 1..=7`).
const RUN_FOR: RunFor = RunFor::Duration(Duration::from_secs(5));
const HISTORICAL: RunMode = RunMode::HistoricalFrom(NanoTime::ZERO);

/// Assemble the live price-book graph and return the builder plus the book
/// handle. The scenario is fully synthetic and deterministic, driven off a
/// per-cycle counter `n = 1, 2, 3, …`:
///
/// - a **price feed** emits one `(instrument, price)` each cycle, cycling the
///   instrument id through `1 → 2 → 3 → 1 → …` with climbing prices;
/// - an **add** stream introduces `inst1`, `inst2`, `inst3` on the first three
///   cycles;
/// - a **del** stream removes `inst1` (cycle 4) and `inst2` (cycle 5), so
///   membership both grows and shrinks *at runtime*.
///
/// [`Builder::dynamic_group`] wires a per-instrument sub-graph on each `add`
/// (filter the feed down to that instrument, project its price), tears it down
/// on each `del`, and folds every live member's *current* price into a
/// [`BTreeMap`] price book — all from inside the running graph, no stop/restart.
fn build() -> (GraphBuilder, Handle<PriceBook>) {
    let g = GraphBuilder::new();

    // 1, 2, 3, … — one tick per cycle.
    let n = g.ticker(Duration::from_secs(1)).count();

    // Price feed: instrument id cycles 1 → 2 → 3 → 1 → …, price climbs so every
    // update is visible.
    let feed = n
        .map(|c: &u64| {
            let inst = (c - 1) % 3 + 1;
            (inst, inst * 100 + c * 10)
        })
        .handle();

    // Lifecycle streams. `map_filter` returns `(key, keep)`: on cycles where
    // `keep` is false the value is dropped, so the group only ever sees the
    // add/del keys we intend. `saturating_sub` keeps the discarded keys from
    // underflowing on early cycles.
    let add = n
        .map_filter(|c: &u64| (*c, *c <= 3)) // add inst1@1, inst2@2, inst3@3
        .handle();
    let del = n
        .map_filter(|c: &u64| (c.saturating_sub(3), *c == 4 || *c == 5)) // del inst1@4, inst2@5
        .handle();

    let book = g.with_builder(|b| {
        b.dynamic_group(
            add,
            del,
            // Factory: per-instrument sub-graph = the feed filtered to this
            // instrument, projected to its price.
            move |ext: &mut Extension<'_>, inst: Instrument| {
                let mine = ext.filter_value(feed, move |(i, _): &(Instrument, Price)| *i == inst);
                ext.map(mine, |(_, px): &(Instrument, Price)| *px)
            },
            PriceBook::new(),
            |book: &mut PriceBook, inst: &Instrument, px: &Price| {
                book.insert(*inst, *px);
            },
            |book: &mut PriceBook, inst: &Instrument| {
                book.remove(inst);
            },
        )
    });

    (g, book)
}

fn render(book: &PriceBook) -> String {
    let body = book
        .iter()
        .map(|(inst, px)| format!("inst{inst}={px}"))
        .collect::<Vec<_>>()
        .join(", ");
    format!("{{{body}}}")
}

fn main() -> anyhow::Result<()> {
    let (g, book) = build();

    // Print the book each time it ticks. The group ticks its book downstream as
    // membership grows, then once more once the deletes have settled — so the
    // output shows the book both build up and collapse to the surviving
    // instrument.
    let _printed = g.wrap(book).for_each(|book: &PriceBook| {
        println!("price book: {}", render(book));
        Ok(())
    });

    // `dynamic_group` splices its per-instrument sub-graphs between cycles, so
    // the run is driven with `run_dynamic`. The hook here is a no-op — all the
    // add/remove logic lives inside the group, driven by its `add`/`del` edges.
    let mut runner = g.build();
    runner.run_dynamic(HISTORICAL, RUN_FOR, |_ext, _cycle| Ok(()))?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The sequence of book states the group ticks downstream. It emits as each
    /// instrument is added (the book grows), then once more after the deletes
    /// leave only `inst3` live — its price being the last one fed to it before
    /// the run ended:
    ///
    /// | tick | event                    | book emitted            |
    /// |------|--------------------------|-------------------------|
    /// | 1    | add inst1, feed (1,110)  | `{inst1=110}`           |
    /// | 2    | add inst2, feed (2,220)  | `{inst1=110,inst2=220}` |
    /// | 3    | add inst3, feed (3,330)  | `{inst1=110,inst2=220,inst3=330}` |
    /// | 4–7  | del inst1/inst2, feeds   | `{inst3=360}`           |
    fn expected_states() -> Vec<PriceBook> {
        let b = |pairs: &[(u64, u64)]| pairs.iter().copied().collect::<PriceBook>();
        vec![
            b(&[(1, 110)]),
            b(&[(1, 110), (2, 220)]),
            b(&[(1, 110), (2, 220), (3, 330)]),
            b(&[(3, 360)]),
        ]
    }

    #[test]
    fn price_book_tracks_runtime_membership() {
        let (g, book) = build();
        let states = g.wrap(book).accumulate();
        let mut runner = g.build();
        runner
            .run_dynamic(HISTORICAL, RUN_FOR, |_ext, _cycle| Ok(()))
            .unwrap();

        assert_eq!(runner.value(&states), expected_states(), "book states");
        assert_eq!(
            runner.value(book),
            PriceBook::from([(3, 360)]),
            "final book holds only the surviving instrument"
        );
    }
}
