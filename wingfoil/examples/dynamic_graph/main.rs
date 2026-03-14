//! Demonstrates dynamic graph construction using `state.add_upstream()` and
//! `state.remove_node()`.
//!
//! Three streams drive the system:
//! - `new_instrument` — fires when an instrument is added
//! - `del_instrument` — fires when an instrument is deleted
//! - `inst_price`     — continuous price feed, cycling through instrument IDs
//!
//! A [`PriceAggregator`] node wires in per-instrument subgraphs on demand via
//! `state.add_upstream()`, and tears them down via `state.remove_node()`.
//!
//! Target scenario: add inst0 → inst1 → inst2 → inst3 → delete inst1 → add inst4.
//! Final price book: {inst0, inst2, inst3, inst4}.
//!
//! This example targets the graph-dynamism API described in
//! `PLAN-graph-dynamism.md` (issue #54).

mod demux;
mod source;

use log::Level::Info;
use std::collections::BTreeMap;
use std::rc::Rc;
use std::time::Duration;

use wingfoil::*;

use source::{Instrument, Price};

// ---------------------------------------------------------------------------
// Per-instrument processing
// ---------------------------------------------------------------------------

/// Applies a transformation to one instrument's price stream.
fn process(stream: Rc<dyn Stream<(Instrument, Price)>>) -> Rc<dyn Stream<(Instrument, Price)>> {
    stream.map(|(inst, px)| (inst, (px * 100.0).round()))
}

// ---------------------------------------------------------------------------
// InstrumentStreams
// ---------------------------------------------------------------------------

/// Bundles the two nodes kept per instrument (needed for `remove_node`).
struct InstrumentStreams {
    filtered: Rc<dyn Stream<(Instrument, Price)>>,
    processed: Rc<dyn Stream<(Instrument, Price)>>,
}

// ---------------------------------------------------------------------------
// PriceAggregator
// ---------------------------------------------------------------------------

struct PriceAggregator {
    /// Source price stream — stored to build per-instrument filters in `cycle()`.
    inst_price: Rc<dyn Stream<(Instrument, Price)>>,
    /// Active upstream: ticks when a new instrument is added.
    new_instrument: Rc<dyn Stream<Instrument>>,
    /// Active upstream: ticks when an instrument is deleted.
    del_instrument: Rc<dyn Stream<Instrument>>,
    /// Dynamic per-instrument streams, keyed by instrument name.
    per_instrument: BTreeMap<Instrument, InstrumentStreams>,
    /// Current price book — the stream's output value.
    value: BTreeMap<Instrument, Price>,
}

impl PriceAggregator {
    fn new(
        inst_price: Rc<dyn Stream<(Instrument, Price)>>,
        new_instrument: Rc<dyn Stream<Instrument>>,
        del_instrument: Rc<dyn Stream<Instrument>>,
    ) -> Self {
        Self {
            inst_price,
            new_instrument,
            del_instrument,
            per_instrument: BTreeMap::new(),
            value: BTreeMap::new(),
        }
    }
}

impl MutableNode for PriceAggregator {
    fn upstreams(&self) -> UpStreams {
        // Both `new_instrument` and `del_instrument` are static active upstreams.
        // Per-instrument processed streams are registered dynamically via
        // `state.add_upstream()` during `cycle()`.
        UpStreams::new(
            vec![
                self.new_instrument.clone().as_node(),
                self.del_instrument.clone().as_node(),
            ],
            vec![],
        )
    }

    fn cycle(&mut self, state: &mut GraphState) -> anyhow::Result<bool> {
        // Wire a new per-instrument subgraph whenever a new instrument arrives.
        if state.ticked(self.new_instrument.clone().as_node()) {
            let instrument = self.new_instrument.peek_value();
            let inst_key = instrument.clone();

            // Build a filtered + processed sub-stream for this instrument.
            let filtered = self
                .inst_price
                .filter_value(move |(inst, _)| inst == &inst_key);
            let processed = process(filtered.clone());

            // Queue the subgraph as an active upstream — wired at cycle boundary.
            // `recycle: true` schedules an immediate callback on the new upstream
            // so it fires on the very next engine iteration, letting the aggregator
            // catch the price that triggered the add_upstream call.
            state.add_upstream(processed.clone().as_node(), true, true);
            self.per_instrument.insert(
                instrument,
                InstrumentStreams {
                    filtered,
                    processed,
                },
            );
        }

        // Remove the per-instrument subgraph when an instrument is deleted.
        if state.ticked(self.del_instrument.clone().as_node()) {
            let instrument = self.del_instrument.peek_value();
            if let Some(streams) = self.per_instrument.remove(&instrument) {
                state.remove_node(streams.processed.clone().as_node());
                state.remove_node(streams.filtered.clone().as_node());
                self.value.remove(&instrument);
            }
        }

        // Collect latest prices from dynamic upstreams that fired this cycle.
        //
        // Note: `state.ticked()` returns `false` for nodes not yet registered
        // (e.g. streams added via `add_upstream` in this same cycle), so newly
        // wired subgraphs are silently skipped until the next engine cycle.
        let mut ticked = false;
        for (inst, streams) in &self.per_instrument {
            if state.ticked(streams.processed.clone().as_node()) {
                let (_inst, price) = streams.processed.peek_value();
                self.value.insert(inst.clone(), price);
                ticked = true;
            }
        }

        Ok(ticked)
    }
}

impl StreamPeekRef<BTreeMap<Instrument, Price>> for PriceAggregator {
    fn peek_ref(&self) -> &BTreeMap<Instrument, Price> {
        &self.value
    }
}

// ---------------------------------------------------------------------------
// main
// ---------------------------------------------------------------------------

fn main() -> anyhow::Result<()> {
    env_logger::init();

    let period = Duration::from_secs(1);
    let src = source::source(period);

    // Log lifecycle events; the lifecycle stream already emits exactly once per
    // add/delete event (driven by a dedicated ticker), so no distinct() is needed.
    let new_instrument = src.new_instrument.logged("new instrument", Info);
    let del_instrument = src.del_instrument.logged("del instrument", Info);

    // The aggregator is a stream of price books: BTreeMap<Instrument, Price>.
    let aggregator = PriceAggregator::new(src.inst_price, new_instrument, del_instrument)
        .into_stream()
        .logged("price book", Info);

    aggregator.run(RunMode::HistoricalFrom(NanoTime::ZERO), RunFor::Cycles(20))?;

    // Alternative implementation using demux_it for per-instrument routing.
    demux::run(period, 20)
}
