#![doc = include_str!("./README.md")]
//!
//! ```sh
//! cargo run -p wingfoil --example dynamic --features dynamic-graph
//! ```

#[path = "../market_data.rs"]
mod market_data;

use std::collections::BTreeMap;
use std::time::Duration;

use wingfoil::interp::{Extension, Handle};
use wingfoil::prelude::*;
use wingfoil::{NanoTime, RunFor, RunMode};

use market_data::{Instrument, Price, market_data};

type PriceBook = BTreeMap<Instrument, Price>;

const PERIOD: Duration = Duration::from_secs(1);
const HISTORICAL: RunMode = RunMode::HistoricalFrom(NanoTime::ZERO);

/// The run is bounded by wall-clock time rather than a cycle count: splicing a
/// sub-graph in or out consumes scheduler cycles, so `RunFor::Cycles(n)` would
/// *not* map cleanly to `n` ticker ticks. Both tickers fire at `t = 0, 1, …,
/// 19s`, so a 19-second bound yields exactly the legacy oracle's `n = 1..=20`.
const RUN_FOR: RunFor = RunFor::Duration(Duration::from_secs(19));

/// Round a raw price to the aggregator's storage form (2dp, scaled by 100) —
/// legacy's `process()`.
fn round(price: Price) -> Price {
    (price * 100.0).round()
}

/// Wire the price book with [`Builder::dynamic_group`], the wingfoil twin of
/// legacy `dynamic_group_stream`: an `add` stream, a `del` stream, and a
/// factory that builds a per-instrument sub-graph. On each `add` it is spliced
/// onto the **running** graph; on each `del` it is torn down; every cycle each
/// live member's current value is folded into a [`BTreeMap`] price book.
///
/// [`Builder::dynamic_group`]: wingfoil::interp::Builder::dynamic_group
fn build() -> (GraphBuilder, Handle<PriceBook>) {
    let g = GraphBuilder::new();
    let src = market_data(&g, PERIOD);
    let feed = src.inst_price.handle();

    let book = g.with_builder(|b| {
        b.dynamic_group(
            src.new_instrument.handle(),
            src.del_instrument.handle(),
            // Per-instrument sub-graph: the shared feed filtered down to this
            // instrument, projected to its rounded price.
            move |ext: &mut Extension<'_>, inst: Instrument| {
                let mine = ext.filter_value(feed, move |(i, _): &(Instrument, Price)| *i == inst);
                ext.map(mine, |(_, px): &(Instrument, Price)| round(*px))
            },
            PriceBook::new(),
            |book: &mut PriceBook, inst: &Instrument, px: &Price| {
                book.insert(inst.clone(), *px);
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
        .map(|(inst, px)| format!("{inst}={px}"))
        .collect::<Vec<_>>()
        .join(", ");
    format!("{{{body}}}")
}

fn main() -> anyhow::Result<()> {
    let (g, book) = build();

    let _printed = g.wrap(book).for_each(|book: &PriceBook| {
        println!("price book (dynamic_group): {}", render(book));
        Ok(())
    });

    // `dynamic_group` splices its per-instrument sub-graphs between cycles, so
    // the run is driven with `run_dynamic`. The hook is a no-op — every add and
    // remove is driven by the group's own `add`/`del` edges.
    let mut runner = g.build();
    runner.run_dynamic(HISTORICAL, RUN_FOR, |_ext, _cycle| Ok(()))?;
    Ok(())
}

#[cfg(test)]
#[path = "../oracle.rs"]
mod oracle;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn price_book_matches_the_legacy_oracle() {
        let (g, book) = build();
        let states = g.wrap(book).accumulate();
        let mut runner = g.build();
        runner
            .run_dynamic(HISTORICAL, RUN_FOR, |_ext, _cycle| Ok(()))
            .unwrap();

        assert_eq!(
            runner.value(&states),
            oracle::dynamic_book_states(),
            "book states"
        );
    }
}
