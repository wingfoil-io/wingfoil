//! Demonstrates dynamic graph construction using `state.add_upstream()` and
//! `state.remove_node()`.
//!
//! A price source emits `(Instrument, Price)` pairs, cycling through an
//! increasing number of instruments over time. A [`PriceAggregator`] node
//! maintains a sliding window of at most [`MAX_INSTRUMENTS`] live instruments:
//!
//! - When a new instrument is discovered it wires in a per-instrument
//!   processing subgraph via `state.add_upstream()`.
//! - When the window is full, the oldest instrument is evicted via
//!   `state.remove_node()` before the new one is added.
//!
//! This example targets the graph-dynamism API described in
//! `PLAN-graph-dynamism.md` (issue #54).

use log::Level::Info;
use std::collections::{BTreeMap, VecDeque};
use std::rc::Rc;
use std::time::Duration;

use wingfoil::*;

type Instrument = String;
type Price = f64;

/// Maximum number of concurrently live per-instrument subgraphs.
const MAX_INSTRUMENTS: usize = 4;

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

struct InstrumentStreams {
    /// Filtered view of source — stored so we can remove_node it on eviction.
    filtered: Rc<dyn Stream<(Instrument, Price)>>,
    /// Processed (transformed) stream — the one we add_upstream and peek.
    processed: Rc<dyn Stream<(Instrument, Price)>>,
}

struct PriceAggregator {
    /// Source stream — stored to build per-instrument filters in `cycle()`.
    source: Rc<dyn Stream<(Instrument, Price)>>,
    /// Active upstream: ticks only when a new (distinct) instrument is seen.
    new_instruments: Rc<dyn Stream<Instrument>>,
    /// Per-instrument streams, keyed by instrument name.
    per_instrument: BTreeMap<Instrument, InstrumentStreams>,
    /// Insertion-order queue — drives eviction when the window is full.
    instrument_order: VecDeque<Instrument>,
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
            instrument_order: VecDeque::new(),
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
        // When a new instrument is discovered, optionally evict the oldest to
        // keep the window at MAX_INSTRUMENTS, then wire in the new subgraph.
        if state.ticked(self.new_instruments.clone().as_node()) {
            let instrument = self.new_instruments.peek_value();

            // Evict the oldest instrument when the window is full.
            if self.instrument_order.len() >= MAX_INSTRUMENTS {
                let evicted = self.instrument_order.pop_front().unwrap();
                if let Some(streams) = self.per_instrument.remove(&evicted) {
                    state.remove_node(streams.processed.clone().as_node());
                    state.remove_node(streams.filtered.clone().as_node());
                    self.value.remove(&evicted);
                }
            }

            // Build and wire the new per-instrument subgraph.
            let inst_key = instrument.clone();
            let filtered = self
                .source
                .filter_value(move |(inst, _px)| inst == &inst_key);
            let processed = process(filtered.clone());

            // `recycle: true` schedules an immediate callback so the new
            // subgraph catches the price that triggered this add_upstream call.
            state.add_upstream(processed.clone().as_node(), true, true);
            self.per_instrument.insert(
                instrument.clone(),
                InstrumentStreams {
                    filtered,
                    processed,
                },
            );
            self.instrument_order.push_back(instrument);
        }

        // Collect latest prices from dynamic upstreams that fired this cycle.
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

    aggregator.run(RunMode::HistoricalFrom(NanoTime::ZERO), RunFor::Cycles(40))
}
