#![doc = include_str!("./README.md")]
//!
//! ```sh
//! cargo run --manifest-path crates/wingfoil/Cargo.toml --example demux --features dynamic-graph
//! ```

use std::collections::BTreeMap;
use std::time::Duration;

use wingfoil::interp::DemuxEvent;
use wingfoil::prelude::*;
use wingfoil::{NanoTime, RunFor, RunMode};

type Instrument = u64;
type Price = u64;
type PriceBook = BTreeMap<Instrument, Price>;

/// Lifecycle + price events for one instrument, carried on a single stream so
/// simultaneous price/delete ticks travel together in one burst.
#[derive(Clone, Debug, Default, PartialEq)]
enum InstEvent {
    #[default]
    None,
    Price(Instrument, Price),
    Delete(Instrument),
}

/// The instrument an event refers to (the demux routing key).
fn inst_key(event: &InstEvent) -> Instrument {
    match event {
        InstEvent::Price(inst, _) | InstEvent::Delete(inst) => *inst,
        InstEvent::None => 0,
    }
}

/// Pool of per-instrument slots. Peak concurrency here is 3 (inst1/2/3 before
/// the deletes), so 4 leaves headroom and the overflow child never fires.
const CAPACITY: usize = 4;

/// Assemble a price book fed through a **statically-wired** demux, and return
/// the builder plus the book stream. Unlike the `dynamic` example (which splices
/// per-instrument sub-graphs onto a live graph), the topology here is fixed: a
/// fixed pool of slots is wired once, and `demux_it` routes each event to its
/// instrument's slot at runtime, recycling a slot when its instrument is
/// deleted. No `run_dynamic`, no graph mutation.
///
/// The scenario is synthetic and deterministic, driven off a counter `n`:
/// a price feed cycles instrument ids `1 → 2 → 3 → 1 → …`, and delete events
/// retire `inst1` (cycle 4) and `inst2` (cycle 5).
fn build() -> (GraphBuilder, Stream<PriceBook>) {
    let g = GraphBuilder::new();

    let n = g.ticker(Duration::from_secs(1)).count();

    // One price per cycle for a cycling instrument, prices climbing.
    let prices = n.map(|c: &u64| {
        let inst = (c - 1) % 3 + 1;
        InstEvent::Price(inst, inst * 100 + c * 10)
    });
    // Delete inst1 on cycle 4 and inst2 on cycle 5. `saturating_sub` keeps the
    // discarded keys from underflowing on early cycles.
    let deletes =
        n.map_filter(|c: &u64| (InstEvent::Delete(c.saturating_sub(3)), *c == 4 || *c == 5));

    // Merge the two event streams so a price and a delete landing on the same
    // cycle ride one burst — `demux_it` then routes each item independently.
    let events = g.combine(&[prices, deletes]).handle();

    // Route each event to its instrument's slot; a `Delete` closes (recycles)
    // the slot so resource use tracks *concurrent* instruments, not all-time.
    let (slots, overflow) = g.with_builder(|b| {
        b.demux_it(events, CAPACITY, |event: &InstEvent| {
            let demux = match event {
                InstEvent::Delete(_) => DemuxEvent::Close,
                _ => DemuxEvent::None,
            };
            (inst_key(event), demux)
        })
    });

    // Recombine every slot (plus overflow, which stays empty here), flatten the
    // per-slot bursts back into one event burst, and fold into the price book.
    let mut streams: Vec<Stream<Burst<InstEvent>>> = slots.iter().map(|&h| g.wrap(h)).collect();
    streams.push(g.wrap(overflow));

    let book = g
        .combine(&streams)
        .map(|rows: &Burst<Burst<InstEvent>>| {
            rows.iter().flatten().cloned().collect::<Burst<InstEvent>>()
        })
        .fold(
            PriceBook::new(),
            |book: &mut PriceBook, events: &Burst<InstEvent>| {
                for event in events {
                    match event {
                        InstEvent::Price(inst, px) => {
                            book.insert(*inst, *px);
                        }
                        InstEvent::Delete(inst) => {
                            book.remove(inst);
                        }
                        InstEvent::None => {}
                    }
                }
            },
        );

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

    let _printed = book.for_each(|book: &PriceBook| {
        println!("price book: {}", render(book));
        Ok(())
    });

    let mut runner = g.build();
    runner.run(RunMode::HistoricalFrom(NanoTime::ZERO), RunFor::Cycles(6))?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Static demux ticks the book **every** cycle, so the sequence shows the
    /// book both grow (instruments discovered from price events) and shrink
    /// (slots recycled on delete):
    ///
    /// | cycle | events                       | book after            |
    /// |-------|------------------------------|-----------------------|
    /// | 1     | Price(1,110)                 | `{inst1=110}`         |
    /// | 2     | Price(2,220)                 | `{inst1=110,inst2=220}` |
    /// | 3     | Price(3,330)                 | `{inst1=110,inst2=220,inst3=330}` |
    /// | 4     | Price(1,140), Delete(1)      | `{inst2=220,inst3=330}` |
    /// | 5     | Price(2,250), Delete(2)      | `{inst3=330}`         |
    /// | 6     | Price(3,360)                 | `{inst3=360}`         |
    fn expected_states() -> Vec<PriceBook> {
        let b = |pairs: &[(u64, u64)]| pairs.iter().copied().collect::<PriceBook>();
        vec![
            b(&[(1, 110)]),
            b(&[(1, 110), (2, 220)]),
            b(&[(1, 110), (2, 220), (3, 330)]),
            b(&[(2, 220), (3, 330)]),
            b(&[(3, 330)]),
            b(&[(3, 360)]),
        ]
    }

    #[test]
    fn price_book_routes_through_demux_slots() {
        let (g, book) = build();
        let states = book.accumulate();
        let mut runner = g.build();
        runner
            .run(RunMode::HistoricalFrom(NanoTime::ZERO), RunFor::Cycles(6))
            .unwrap();

        assert_eq!(runner.value(&states), expected_states(), "book states");
        assert_eq!(
            runner.value(&book),
            PriceBook::from([(3, 360)]),
            "final book after the deletes"
        );
    }
}
