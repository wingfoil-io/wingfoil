#![doc = include_str!("./README.md")]
//!
//! ```sh
//! cargo run --manifest-path crates/wingfoil/Cargo.toml --example demux_map
//! ```

#[path = "../market_data.rs"]
mod market_data;

use std::collections::BTreeMap;
use std::time::Duration;

use anyhow::bail;

use wingfoil::interp::DemuxEvent;
use wingfoil::prelude::*;
use wingfoil::{NanoTime, RunFor, RunMode};

use market_data::{Instrument, Price, market_data_offset};

type PriceBook = BTreeMap<Instrument, Price>;

const PERIOD: Duration = Duration::from_secs(1);

/// The lifecycle feed is phase-shifted by half a period so a price and a delete
/// never land on the same cycle — see the module docs above for why the
/// single-value demux APIs need that and `demux_it` does not.
const OFFSET: Duration = Duration::from_millis(500);

const HISTORICAL: RunMode = RunMode::HistoricalFrom(NanoTime::ZERO);

/// Cycles no longer map one-to-one onto price ticks (the offset feed inserts
/// its own), so the run is bounded by time: prices at `t = 0..19s`, deletes at
/// `t = 2.5, 5.5, … 17.5s`.
const RUN_FOR: RunFor = RunFor::Duration(Duration::from_secs(19));

/// Pool of per-instrument slots; peak concurrency stays well under it.
const CAPACITY: usize = 10;

/// Lifecycle + price events for one instrument on a single stream — as in the
/// `demux` example, except that here at most one ever ticks per cycle.
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

fn round(price: Price) -> Price {
    (price * 100.0).round()
}

/// Wire the price book with
/// [`Builder::demux_map`](wingfoil::interp::Builder::demux_map) — the keyed,
/// **single-value** demux. Same fixed slot pool and same [`DemuxEvent`] key
/// lifecycle as `demux_it`, but it routes one value per cycle to one child
/// rather than fanning a burst's items across several. The events are merged
/// with [`combine`](GraphBuilder::combine) and immediately
/// [`collapse`](StreamOps::collapse)d back to a single value — legitimate only
/// because the offset feed guarantees the burst never holds more than one item.
fn build() -> (GraphBuilder, Stream<PriceBook>, Stream<InstEvent>) {
    let g = GraphBuilder::new();
    let src = market_data_offset(&g, PERIOD, OFFSET);

    let price_events = src
        .inst_price
        .map(|(inst, px): &(Instrument, Price)| InstEvent::Price(inst.clone(), *px));
    let del_events = src
        .del_instrument
        .map(|inst: &Instrument| InstEvent::Delete(inst.clone()));
    let events = g.combine(&[price_events, del_events]).collapse().handle();

    let (slots, overflow) = g.with_builder(|b| {
        b.demux_map(events, CAPACITY, |event: &InstEvent| {
            let demux = match event {
                InstEvent::Delete(_) => DemuxEvent::Close,
                _ => DemuxEvent::None,
            };
            (inst_key(event), demux)
        })
    });

    // Per-slot processing: each slot only ever sees one instrument's events.
    let processed: Vec<Stream<InstEvent>> = slots
        .iter()
        .map(|&slot| {
            g.wrap(slot).map(|event: &InstEvent| match event {
                InstEvent::Price(inst, px) => InstEvent::Price(inst.clone(), round(*px)),
                other => other.clone(),
            })
        })
        .collect();

    // Exactly one slot fires per cycle, so the combined burst is a single event
    // — but folding the burst keeps this identical in shape to `demux_it`.
    let book = g.combine(&processed).fold(
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

    // Same obligation as `demux_it`: a key arriving with the pool full routes
    // here, and an unwired overflow child would swallow it silently.
    let _overflowed = overflow.for_each(|event: &InstEvent| {
        bail!("demux overflow: {CAPACITY} slots exhausted, unrouted {event:?}")
    });

    // Tick times are printed too: they are what makes the interleave visible —
    // prices on the second, deletes on the half second.
    let _printed = book
        .with_time()
        .for_each(|(t, book): &(NanoTime, PriceBook)| {
            let secs = f64::from(*t) * NanoTime::SECONDS_PER_NANO;
            println!("t={secs:>5.1}s  price book (demux_map): {}", render(book));
            Ok(())
        });

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
    fn price_book_matches_the_offset_oracle() {
        let (g, book, overflow) = build();
        let states = book.accumulate();
        let overflowed = overflow.accumulate();
        let mut runner = g.build();
        runner.run(HISTORICAL, RUN_FOR).unwrap();

        assert_eq!(
            runner.value(&states),
            oracle::offset_book_states(),
            "book states"
        );
        assert!(
            runner.value(&overflowed).is_empty(),
            "capacity {CAPACITY} covers peak concurrency; overflow must never fire"
        );
    }
}
