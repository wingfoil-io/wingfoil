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

#[path = "../market_data.rs"]
mod source;

use std::collections::BTreeMap;
use std::rc::Rc;
use std::time::Duration;

use log::Level::Info;
use wingfoil::*;

use self::source::{Instrument, Price};
const CAPACITY: usize = 10;
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

pub fn build(period: Duration) -> (Rc<dyn Stream<PriceBook>>, Rc<dyn Node>) {
    let src = self::source::market_data(period);
    let price_events = src.inst_price.map(|(i, p)| InstEvent::Price(i, p));
    let del_events = src.del_instrument.map(InstEvent::Delete);
    let all_events = combine(vec![price_events, del_events]);
    let (slots, overflow) = all_events.demux_it(CAPACITY, |event| {
        let key = inst_key(event);
        let de = match event {
            InstEvent::Delete(_) => DemuxEvent::Close,
            _ => DemuxEvent::None,
        };
        (key, de)
    });
    let overflow_node = overflow.panic();
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

fn main() -> anyhow::Result<()> {
    env_logger::init();
    run(Duration::from_secs(1), 20)
}

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
    use wingfoil::{Graph, NanoTime, RunFor, RunMode};

    fn b(pairs: &[(&str, f64)]) -> BTreeMap<Instrument, Price> {
        pairs.iter().map(|&(k, v)| (k.into(), v)).collect()
    }

    /// Expected price-book state after each emission over 20 cycles.
    ///
    /// Unlike the dynamic-group / dynamic-manual approaches, demux discovers
    /// instruments from price events rather than an explicit add stream.  At n=3
    /// the combined burst carries both Price(inst3, 303.0) *and* Delete(inst1),
    /// so demux opens a slot for inst3 immediately and the book emits — giving
    /// 20 states (one per cycle) instead of 19.
    ///
    /// From n=6 onward the state matches the other approaches exactly.
    fn expected_book_states() -> Vec<BTreeMap<Instrument, Price>> {
        vec![
            b(&[("inst1", 101.0)]),                   // n=1
            b(&[("inst1", 101.0), ("inst2", 202.0)]), // n=2
            b(&[("inst2", 202.0), ("inst3", 303.0)]), // n=3: inst3 slot opened via price
            b(&[("inst2", 204.0), ("inst3", 303.0)]), // n=4
            b(&[("inst2", 204.0), ("inst3", 305.0)]), // n=5
            b(&[("inst3", 305.0), ("inst4", 406.0)]), // n=6
            b(&[("inst3", 307.0), ("inst4", 406.0)]), // n=7
            b(&[("inst3", 307.0), ("inst4", 408.0)]), // n=8
            b(&[("inst4", 408.0), ("inst5", 509.0)]), // n=9
            b(&[("inst4", 410.0), ("inst5", 509.0)]), // n=10
            b(&[("inst4", 410.0), ("inst5", 511.0)]), // n=11
            b(&[("inst5", 511.0), ("inst6", 612.0)]), // n=12
            b(&[("inst5", 513.0), ("inst6", 612.0)]), // n=13
            b(&[("inst5", 513.0), ("inst6", 614.0)]), // n=14
            b(&[("inst6", 614.0), ("inst7", 715.0)]), // n=15
            b(&[("inst6", 616.0), ("inst7", 715.0)]), // n=16
            b(&[("inst6", 616.0), ("inst7", 717.0)]), // n=17
            b(&[("inst7", 717.0), ("inst8", 818.0)]), // n=18
            b(&[("inst7", 719.0), ("inst8", 818.0)]), // n=19
            b(&[("inst7", 719.0), ("inst8", 820.0)]), // n=20
        ]
    }

    /// Demux has no recycle cycles, so RunFor::Cycles(20) maps cleanly to n=1..20.
    #[test]
    fn price_book_accumulation() {
        let period = std::time::Duration::from_secs(1);
        let (price_book, overflow_node) = build(period);
        let assertion = price_book.accumulate().finally(|states, _| {
            assert_eq!(states, expected_book_states());
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
