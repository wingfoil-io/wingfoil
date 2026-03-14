//! Demonstrates dynamic graph construction using `state.add_upstream()`.
//!
//! A price source emits `(Instrument, Price)` pairs, cycling through an
//! increasing number of instruments over time. A [`PriceAggregator`] node
//! detects new instruments and wires in a per-instrument processing subgraph
//! on demand by calling `state.add_upstream()` from within `cycle()`.
//!
//! This example targets the graph-dynamism API described in
//! `PLAN-graph-dynamism.md` (issue #54).

use log::Level::Info;
use std::collections::BTreeMap;
use std::rc::Rc;
use std::time::Duration;

use wingfoil::*;

type Instrument = String;
type Price = f64;

// ---------------------------------------------------------------------------
// Source
// ---------------------------------------------------------------------------

/// Emits `(Instrument, Price)` pairs. A new distinct instrument is introduced
/// roughly every `period * 3`, so the set of live instruments grows over time.
fn source(period: Duration) -> Rc<dyn Stream<(Instrument, Price)>> {
    let n_prices = ticker(period).count();
    let n_instruments = ticker(period * 3).count();
    bimap(
        Dep::Active(n_prices),
        Dep::Passive(n_instruments),
        |i_price: u64, i_inst: u64| {
            // Avoid divide-by-zero: n_instruments defaults to 0 before first tick.
            let n = i_inst.max(1);
            let inst_id = i_price % n;
            let price = inst_id as f64 + i_price as f64 / 100.0;
            let inst = format!("inst_{inst_id}");
            (inst, price)
        },
    )
}

// ---------------------------------------------------------------------------
// Per-instrument processing
// ---------------------------------------------------------------------------

/// Applies a transformation to one instrument's price stream.
fn process(stream: Rc<dyn Stream<(Instrument, Price)>>) -> Rc<dyn Stream<(Instrument, Price)>> {
    stream.map(|(inst, px)| (inst, (px * 100.0).round()))
}

// ---------------------------------------------------------------------------
// PriceAggregator
// ---------------------------------------------------------------------------

struct PriceAggregator {
    /// Source stream — stored to build per-instrument filters in `cycle()`.
    source: Rc<dyn Stream<(Instrument, Price)>>,
    /// Active upstream: ticks only when a new (distinct) instrument is seen.
    new_instruments: Rc<dyn Stream<Instrument>>,
    /// Dynamic per-instrument processed streams, keyed by instrument name.
    per_instrument: BTreeMap<Instrument, Rc<dyn Stream<(Instrument, Price)>>>,
    /// Current price book — the stream's output value.
    value: BTreeMap<Instrument, Price>,
}

impl PriceAggregator {
    fn new(
        source: Rc<dyn Stream<(Instrument, Price)>>,
        new_instruments: Rc<dyn Stream<Instrument>>,
    ) -> Self {
        Self {
            source,
            new_instruments,
            per_instrument: BTreeMap::new(),
            value: BTreeMap::new(),
        }
    }
}

impl MutableNode for PriceAggregator {
    fn upstreams(&self) -> UpStreams {
        // `new_instruments` is the only *static* active upstream.
        // Per-instrument processed streams are registered dynamically via
        // `state.add_upstream()` during `cycle()`.
        UpStreams::new(vec![self.new_instruments.clone().as_node()], vec![])
    }

    fn cycle(&mut self, state: &mut GraphState) -> anyhow::Result<bool> {
        // Wire a new per-instrument subgraph whenever a new instrument arrives.
        if state.ticked(self.new_instruments.clone().as_node()) {
            let instrument = self.new_instruments.peek_value();
            let inst_key = instrument.clone();

            // Build a filtered + processed sub-stream for this instrument.
            let filtered = self
                .source
                .filter_value(move |(inst, _px)| inst == &inst_key);
            let processed = process(filtered);

            // Queue the subgraph as an active upstream — wired at cycle boundary.
            // After this cycle ends, `processed` will trigger `PriceAggregator`
            // on every future tick relevant to this instrument.
            state.add_upstream(processed.clone().as_node(), true);
            self.per_instrument.insert(instrument, processed);
        }

        // Collect latest prices from dynamic upstreams that fired this cycle.
        //
        // Note: `state.ticked()` returns `false` for nodes not yet registered
        // (e.g. streams added via `add_upstream` in this same cycle), so newly
        // wired subgraphs are silently skipped until the next engine cycle.
        let mut ticked = false;
        for (inst, stream) in &self.per_instrument {
            if state.ticked(stream.clone().as_node()) {
                let (_inst, price) = stream.peek_value();
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
    let src = source(period);

    // Detect new instruments: map to name, deduplicate, log arrivals.
    let new_instruments = src
        .map(|(inst, _px)| inst)
        .distinct()
        .logged("new instrument", Info);

    // The aggregator is a stream of price books: BTreeMap<Instrument, Price>.
    let aggregator = PriceAggregator::new(src, new_instruments)
        .into_stream()
        .logged("price book", Info);

    aggregator.run(RunMode::HistoricalFrom(NanoTime::ZERO), RunFor::Cycles(20))
}
