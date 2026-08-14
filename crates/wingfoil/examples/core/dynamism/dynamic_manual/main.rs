#![doc = include_str!("./README.md")]
//!
//! ```sh
//! cargo run -p wingfoil --example dynamic_manual --features dynamic-graph
//! ```

#[path = "../market_data.rs"]
mod market_data;

use std::cell::RefCell;
use std::collections::BTreeMap;
use std::rc::Rc;
use std::time::Duration;

use wingfoil::interp::{Extension, Handle};
use wingfoil::prelude::*;
use wingfoil::{NanoTime, RunFor, RunMode};

use market_data::{Instrument, Price, market_data};

type PriceBook = BTreeMap<Instrument, Price>;

const PERIOD: Duration = Duration::from_secs(1);
const HISTORICAL: RunMode = RunMode::HistoricalFrom(NanoTime::ZERO);

/// Same bound as the `dynamic` example, and for the same reason: splicing
/// consumes scheduler cycles, so only a time bound maps cleanly onto the
/// oracle's `n = 1..=20`.
const RUN_FOR: RunFor = RunFor::Duration(Duration::from_secs(19));

fn round(price: Price) -> Price {
    (price * 100.0).round()
}

/// The per-instrument sub-graph `Driver` splices in, kept so it can be torn
/// down again: the feed filter and the price projection built on top of it.
#[derive(Clone, Copy)]
struct Live {
    filtered: Handle<(Instrument, Price)>,
    priced: Handle<Price>,
}

/// Everything the `run_dynamic` hook needs to mutate the graph — the
/// hand-rolled equivalent of the bookkeeping [`Builder::dynamic_group`] does
/// internally: the registry of live members, the aggregate, and the node the
/// members are spliced onto.
///
/// Graph nodes cannot be built from inside a `cycle` closure, so the two
/// lifecycle streams deposit their values in `adds` / `dels` from a
/// [`for_each`](StreamOps::for_each) tap, and the hook — which runs *between*
/// cycles, where an [`Extension`] is available — drains them.
///
/// [`Builder::dynamic_group`]: wingfoil::interp::Builder::dynamic_group
struct Driver {
    /// The shared price feed every per-instrument sub-graph filters.
    feed: Handle<(Instrument, Price)>,
    /// The node the per-instrument sub-graphs are spliced onto as active
    /// upstreams, so it re-fires whenever any live member ticks.
    aggregator: Handle<PriceBook>,
    /// Instrument additions seen this cycle, awaiting a splice.
    adds: Rc<RefCell<Vec<Instrument>>>,
    /// Instrument deletions seen this cycle, awaiting a teardown.
    dels: Rc<RefCell<Vec<Instrument>>>,
    /// The aggregate itself: written by each member's projection, read by the
    /// aggregator node.
    book: Rc<RefCell<PriceBook>>,
    /// The live per-instrument sub-graphs, by instrument.
    live: RefCell<BTreeMap<Instrument, Live>>,
}

impl Driver {
    /// One boundary step: splice in every instrument added on the cycle just
    /// finished, then tear down every instrument deleted on it.
    fn step(&self, ext: &mut Extension<'_>) -> anyhow::Result<()> {
        let adds: Vec<Instrument> = self.adds.borrow_mut().drain(..).collect();
        for inst in adds {
            let key = inst.clone();
            let filtered =
                ext.filter_value(self.feed, move |(i, _): &(Instrument, Price)| *i == key);

            // The projection also records into the shared book — the manual
            // stand-in for `dynamic_group`'s `on_tick` fold.
            let key = inst.clone();
            let book = self.book.clone();
            let priced = ext.map(filtered, move |(_, px): &(Instrument, Price)| {
                let px = round(*px);
                book.borrow_mut().insert(key.clone(), px);
                px
            });

            // `recycle = true` schedules the new region to fire at `time + 1`,
            // so it observes the feed's *current* value rather than a `Default`
            // — which is why the first book state carries a real price.
            ext.add_upstream(self.aggregator, priced, true, true);
            self.live
                .borrow_mut()
                .insert(inst, Live { filtered, priced });
        }

        // The book entry is already gone (dropped in-cycle by the `del` tap, so
        // the aggregator never republishes a deleted instrument); all that is
        // left for the boundary is unwiring the nodes.
        let dels: Vec<Instrument> = self.dels.borrow_mut().drain(..).collect();
        for inst in dels {
            let removed = self.live.borrow_mut().remove(&inst);
            if let Some(Live { filtered, priced }) = removed {
                ext.remove(priced)?;
                ext.remove(filtered)?;
            }
        }
        Ok(())
    }
}

/// The low-level equivalent of the `dynamic` example: instead of handing the
/// lifecycle to [`Builder::dynamic_group`], this drives
/// [`Extension::add_upstream`] and [`Extension::remove`] by hand from the
/// `run_dynamic` hook, and keeps its own registry as a plain [`BTreeMap`].
///
/// [`Builder::dynamic_group`]: wingfoil::interp::Builder::dynamic_group
fn build() -> (GraphBuilder, Stream<PriceBook>, Driver) {
    let g = GraphBuilder::new();
    let src = market_data(&g, PERIOD);

    let adds: Rc<RefCell<Vec<Instrument>>> = Rc::new(RefCell::new(Vec::new()));
    let dels: Rc<RefCell<Vec<Instrument>>> = Rc::new(RefCell::new(Vec::new()));
    let book: Rc<RefCell<PriceBook>> = Rc::new(RefCell::new(PriceBook::new()));

    let _tap_add = src.new_instrument.for_each({
        let adds = adds.clone();
        move |inst: &Instrument| {
            adds.borrow_mut().push(inst.clone());
            Ok(())
        }
    });
    // Deletion has to take effect *within* the cycle it is seen, or an
    // instrument whose sibling ticks on that same cycle would be republished
    // one last time. So the tap drops the book entry immediately and leaves
    // only the unwiring to the boundary — the same split `dynamic_group` makes
    // internally between `on_remove` (now) and `remove_node` (staged).
    let tap_del = src.del_instrument.for_each({
        let dels = dels.clone();
        let book = book.clone();
        move |inst: &Instrument| {
            book.borrow_mut().remove(inst);
            dels.borrow_mut().push(inst.clone());
            Ok(())
        }
    });

    // The aggregator has no *active* upstreams at wiring time — every edge that
    // fires it is spliced in at runtime — and publishes the shared book
    // whenever one of those members ticks, which is legacy's
    // `cycle() -> ticked` shape. `tap_del` is declared **passive**: it never
    // triggers the aggregator, but it lifts the aggregator's layer above the
    // tap, so the in-cycle removal above is ordered before the publish by the
    // scheduler rather than by luck.
    let aggregator: Stream<PriceBook> =
        g.custom_node(&[], &[tap_del.upstream()], Activation::NONE, {
            let book = book.clone();
            move |_ctx| Ok(Tick::Value(book.borrow().clone()))
        });

    let driver = Driver {
        feed: src.inst_price.handle(),
        aggregator: aggregator.handle(),
        adds,
        dels,
        book,
        live: RefCell::new(BTreeMap::new()),
    };

    (g, aggregator, driver)
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
    let (g, book, driver) = build();

    let _printed = book.for_each(|book: &PriceBook| {
        println!("price book (dynamic_manual): {}", render(book));
        Ok(())
    });

    let mut runner = g.build();
    runner.run_dynamic(HISTORICAL, RUN_FOR, |ext, _cycle| driver.step(ext))?;
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
        let (g, book, driver) = build();
        let states = book.accumulate();
        let mut runner = g.build();
        runner
            .run_dynamic(HISTORICAL, RUN_FOR, |ext, _cycle| driver.step(ext))
            .unwrap();

        assert_eq!(
            runner.value(&states),
            oracle::dynamic_book_states(),
            "hand-rolled splicing matches `dynamic_group`"
        );
    }
}
