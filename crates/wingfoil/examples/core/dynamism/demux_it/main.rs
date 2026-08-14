#![doc = include_str!("./README.md")]
//!
//! ```sh
//! cargo run --manifest-path crates/wingfoil/Cargo.toml --example demux
//! ```

#[path = "../market_data.rs"]
mod market_data;

use std::collections::BTreeMap;
use std::time::Duration;

use anyhow::bail;

use wingfoil::interp::DemuxEvent;
use wingfoil::prelude::*;
use wingfoil::{NanoTime, RunFor, RunMode};

use market_data::{Instrument, Price, market_data};

type PriceBook = BTreeMap<Instrument, Price>;

const PERIOD: Duration = Duration::from_secs(1);
const HISTORICAL: RunMode = RunMode::HistoricalFrom(NanoTime::ZERO);

/// No graph mutation here, so cycles map one-to-one onto ticker ticks and
/// `RunFor::Cycles` addresses the oracle's `n = 1..=20` directly.
const RUN_FOR: RunFor = RunFor::Cycles(20);

/// Pool of per-instrument slots. Peak concurrency stays well under this, so the
/// overflow child never fires (the test asserts it).
const CAPACITY: usize = 10;

/// Lifecycle + price events for one instrument, carried on a single stream so
/// simultaneous price/delete ticks travel together in one burst.
#[derive(Clone, Debug, Default, PartialEq)]
enum InstEvent {
    #[default]
    None,
    Price(Instrument, Price),
    Delete(Instrument),
}

/// The instrument an event refers to — the demux routing key.
fn inst_key(event: &InstEvent) -> Instrument {
    match event {
        InstEvent::Price(inst, _) | InstEvent::Delete(inst) => inst.clone(),
        InstEvent::None => String::new(),
    }
}

/// Round a raw price to the book's storage form (2dp, scaled by 100) — legacy's
/// `process()`, applied per slot so it sits *inside* the demuxed sub-graph.
fn round(price: Price) -> Price {
    (price * 100.0).round()
}

/// Wire the price book over a **fixed** topology with
/// [`Builder::demux_it`](wingfoil::interp::Builder::demux_it):
///
/// 1. map `inst_price` and `del_instrument` onto a common [`InstEvent`];
/// 2. [`combine`](GraphBuilder::combine) them so simultaneous ticks ride one
///    burst;
/// 3. `demux_it` routes **each item** of that burst to its instrument's slot,
///    and [`DemuxEvent::Close`] recycles a slot when its instrument is deleted,
///    so resource use tracks *concurrent* instruments rather than all-time;
/// 4. round prices per slot, passing `Delete` through untouched;
/// 5. combine every slot, flatten, and fold into the [`BTreeMap`] price book.
///
/// Returns the builder, the book, and the overflow child, which the caller must
/// wire: [`main`] aborts the run on it and the test asserts it never fires.
fn build() -> (GraphBuilder, Stream<PriceBook>, Stream<Burst<InstEvent>>) {
    let g = GraphBuilder::new();
    let src = market_data(&g, PERIOD);

    let price_events = src
        .inst_price
        .map(|(inst, px): &(Instrument, Price)| InstEvent::Price(inst.clone(), *px));
    let del_events = src
        .del_instrument
        .map(|inst: &Instrument| InstEvent::Delete(inst.clone()));
    let all_events = g.combine(&[price_events, del_events]).handle();

    let (slots, overflow) = g.with_builder(|b| {
        b.demux_it(all_events, CAPACITY, |event: &InstEvent| {
            let demux = match event {
                InstEvent::Delete(_) => DemuxEvent::Close,
                _ => DemuxEvent::None,
            };
            (inst_key(event), demux)
        })
    });

    // Per-slot processing: each slot only ever sees one instrument's events.
    let processed: Vec<Stream<Burst<InstEvent>>> = slots
        .iter()
        .map(|&slot| {
            g.wrap(slot).map(|burst: &Burst<InstEvent>| {
                burst
                    .iter()
                    .map(|event| match event {
                        InstEvent::Price(inst, px) => InstEvent::Price(inst.clone(), round(*px)),
                        other => other.clone(),
                    })
                    .collect::<Burst<InstEvent>>()
            })
        })
        .collect();

    let book = g
        .combine(&processed)
        .map(|rows: &Burst<Burst<InstEvent>>| {
            rows.iter().flatten().cloned().collect::<Burst<InstEvent>>()
        })
        .fold(
            PriceBook::new(),
            |book: &mut PriceBook, events: &Burst<InstEvent>| {
                for event in events {
                    match event {
                        InstEvent::Price(inst, px) => {
                            book.insert(inst.clone(), *px);
                        }
                        InstEvent::Delete(inst) => {
                            book.remove(inst);
                        }
                        InstEvent::None => {}
                    }
                }
            },
        );

    let overflow = g.wrap(overflow);
    (g, book, overflow)
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
    let (g, book, overflow) = build();

    let _printed = book.for_each(|book: &PriceBook| {
        println!("price book (demux_it): {}", render(book));
        Ok(())
    });

    // Choosing a capacity up front is what fixed-topology routing costs, so the
    // overflow child is not optional wiring: an event that finds no free slot
    // lands here and must be handled. A `for_each` returning `Err` aborts the
    // run with context — the wingfoil spelling of legacy's `overflow.panic()`.
    let _overflowed = overflow.for_each(|events: &Burst<InstEvent>| {
        bail!("demux overflow: {CAPACITY} slots exhausted, unrouted {events:?}")
    });

    // Fixed topology: a plain `run`, no `run_dynamic`.
    let mut runner = g.build();
    runner.run(HISTORICAL, RUN_FOR)?;
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
        let (g, book, overflow) = build();
        let states = book.accumulate();
        let overflowed = overflow.accumulate();
        let mut runner = g.build();
        runner.run(HISTORICAL, RUN_FOR).unwrap();

        assert_eq!(
            runner.value(&states),
            oracle::demux_book_states(),
            "book states"
        );
        assert!(
            runner.value(&overflowed).is_empty(),
            "capacity {CAPACITY} covers peak concurrency; overflow must never fire"
        );
    }
}
