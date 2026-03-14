//! Alternative price aggregator using `demux_it` instead of the dynamic-graph
//! `state.add_upstream()` / `state.remove_node()` API.
//!
//! The approach:
//! 1. Map both `inst_price` and `del_instrument` to a common [`InstEvent`] enum.
//! 2. [`combine`] the two event streams so simultaneous ticks are both captured.
//! 3. [`demux_it`] routes each event to a fixed-capacity pool of per-instrument
//!    slots; [`DemuxEvent::Close`] recycles a slot when its instrument is deleted,
//!    keeping resource use proportional to the number of *concurrent* instruments.
//! 4. Apply price rounding per slot, passing [`Delete`] events through unchanged.
//! 5. [`combine`] all slots; flatten and [`fold`] into a [`BTreeMap`] price book.
//!
//! [`demux_it`]: wingfoil::StreamOperators::demux_it
//! [`Delete`]: InstEvent::Delete

use std::collections::BTreeMap;
use std::rc::Rc;
use std::time::Duration;

use log::Level::Info;
use wingfoil::*;

use crate::source::{Instrument, Price};

/// Maximum number of concurrently live instruments.
///
/// `demux_it` pre-allocates this many slots. Instrument deletions return slots
/// to the pool via [`DemuxEvent::Close`], so the requirement is
/// `CAPACITY >= max(live instruments at any one time)`, not `>= total ever seen`.
const CAPACITY: usize = 10;

/// Routes both price updates and lifecycle deletions through a single `demux_it`.
#[derive(Clone, Debug, Default)]
enum InstEvent {
    #[default]
    None,
    Price(Instrument, Price),
    Delete(Instrument),
}

fn inst_key(event: &InstEvent) -> Instrument {
    match event {
        InstEvent::Price(i, _) | InstEvent::Delete(i) => i.clone(),
        InstEvent::None => String::new(),
    }
}

type PriceBook = BTreeMap<Instrument, Price>;

/// Builds a price-book stream using `demux_it` for per-instrument routing.
///
/// Returns `(price_book, overflow_node)`.  Both must be included in the graph:
/// `overflow_node` is a sentinel that panics if the demux pool is exhausted.
pub fn build(period: Duration) -> (Rc<dyn Stream<PriceBook>>, Rc<dyn Node>) {
    let src = crate::source::source(period);

    // Map both source streams into the common event type.
    let price_events = src.inst_price.map(|(i, p)| InstEvent::Price(i, p));
    let del_events = src.del_instrument.map(InstEvent::Delete);

    // `combine` batches events from both tickers into one burst per cycle,
    // so a deletion and a price that arrive in the same engine cycle are
    // both captured and routed through the demux.
    let all_events = combine(vec![price_events, del_events]);

    // Route each event to its per-instrument slot.
    // DemuxEvent::Close releases the slot for reuse after a deletion.
    let (slots, overflow) = all_events.demux_it(CAPACITY, |event| {
        let key = inst_key(event);
        let de = match event {
            InstEvent::Delete(_) => DemuxEvent::Close,
            _ => DemuxEvent::None,
        };
        (key, de)
    });
    let overflow_node = overflow.panic(); // CAPACITY is sized to never overflow

    // Apply processing per slot: round prices; pass Delete events through unchanged.
    let processed: Vec<Rc<dyn Stream<Burst<InstEvent>>>> = slots
        .into_iter()
        .map(|slot| {
            slot.map(|burst| {
                burst
                    .into_iter()
                    .map(|event| match event {
                        InstEvent::Price(i, p) => InstEvent::Price(i, (p * 100.0).round()),
                        other => other,
                    })
                    .collect()
            })
        })
        .collect();

    // `combine` all slot streams: fires whenever any slot fires and batches all
    // slot bursts active in this cycle into a single outer burst.
    // Flatten, then fold into the price book.
    let price_book = combine(processed)
        .map(|bursts: Burst<Burst<InstEvent>>| -> Burst<InstEvent> {
            bursts.into_iter().flatten().collect()
        })
        .fold(
            |book: &mut BTreeMap<Instrument, Price>, events: Burst<InstEvent>| {
                for event in events {
                    match event {
                        InstEvent::Price(inst, price) => {
                            book.insert(inst, price);
                        }
                        InstEvent::Delete(inst) => {
                            book.remove(&inst);
                        }
                        InstEvent::None => {}
                    }
                }
            },
        );

    (price_book, overflow_node)
}

/// Runs the demux-based aggregator for `cycles` and logs the final price book.
pub fn run(period: Duration, cycles: u32) -> anyhow::Result<()> {
    let (price_book, overflow_node) = build(period);
    Graph::new(
        vec![
            price_book.logged("price book (demux)", Info).as_node(),
            overflow_node,
        ],
        RunMode::HistoricalFrom(NanoTime::ZERO),
        RunFor::Cycles(cycles),
    )
    .run()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// After 20 cycles the sliding-window price formula (id = (n-1)/3 + (n-1)%3 + 1)
    /// only ever produces ids 1..8.  Of those, inst1..inst6 are deleted by cycle 18,
    /// leaving inst7 (last price at n=19 → 719.0) and inst8 (last price at n=20 → 820.0).
    #[test]
    fn price_book_final_state() {
        let period = std::time::Duration::from_secs(1);
        let (price_book, overflow_node) = build(period);
        let assertion = price_book.finally(|book, _| {
            let expected: BTreeMap<Instrument, Price> =
                [("inst7".into(), 719.0), ("inst8".into(), 820.0)]
                    .into_iter()
                    .collect();
            assert_eq!(book, expected);
            Ok(())
        });
        Graph::new(
            vec![assertion, overflow_node],
            RunMode::HistoricalFrom(NanoTime::ZERO),
            RunFor::Cycles(20),
        )
        .run()
        .unwrap();
    }
}
