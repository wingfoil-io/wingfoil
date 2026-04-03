//! Demonstrates dynamic graph construction by hand-rolling a [`MutableNode`].
//!
//! This is the low-level equivalent of the `dynamism` example: instead of
//! using [`dynamic_group_stream`], `PriceAggregator` directly calls
//! `state.add_upstream()` and `state.remove_node()` and manages its own
//! per-instrument registry as a plain [`BTreeMap`].
//!
//! Three streams drive the system:
//! - `new_instrument` — fires when an instrument is added
//! - `del_instrument` — fires when an instrument is deleted
//! - `inst_price`     — continuous price feed, cycling through instrument IDs

#[path = "../market_data.rs"]
mod source;

use log::Level::Info;
use std::collections::BTreeMap;
use std::rc::Rc;
use std::time::Duration;

use wingfoil::*;

use source::{Instrument, Price};

fn process(stream: Rc<dyn Stream<(Instrument, Price)>>) -> Rc<dyn Stream<(Instrument, Price)>> {
    stream.map(|(inst, px)| (inst, (px * 100.0).round()))
}

struct PriceAggregator {
    inst_price: Rc<dyn Stream<(Instrument, Price)>>,
    new_instrument: Rc<dyn Stream<Instrument>>,
    del_instrument: Rc<dyn Stream<Instrument>>,
    per_instrument: BTreeMap<Instrument, Rc<dyn Stream<(Instrument, Price)>>>,
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

impl WiringPoint for PriceAggregator {
    fn upstreams(&self) -> UpStreams {
        UpStreams::new(
            vec![
                self.new_instrument.clone().as_node(),
                self.del_instrument.clone().as_node(),
            ],
            vec![],
        )
    }
}

impl MutableNode for PriceAggregator {
    fn cycle(&mut self, state: &mut GraphState) -> anyhow::Result<bool> {
        if state.ticked(self.new_instrument.clone().as_node()) {
            let instrument = self.new_instrument.peek_value();
            let inst_key = instrument.clone();
            let processed = process(
                self.inst_price
                    .filter_value(move |(inst, _)| inst == &inst_key),
            );
            state.add_upstream(processed.clone().as_node(), true, true);
            self.per_instrument.insert(instrument, processed);
        }
        if state.ticked(self.del_instrument.clone().as_node()) {
            let instrument = self.del_instrument.peek_value();
            if let Some(processed) = self.per_instrument.remove(&instrument) {
                state.remove_node(processed.as_node());
                self.value.remove(&instrument);
            }
        }
        let mut ticked = false;
        for (inst, processed) in &self.per_instrument {
            if state.ticked(processed.clone().as_node()) {
                let (_inst, price) = processed.peek_value();
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

type PriceBook = BTreeMap<Instrument, Price>;

pub fn build(period: Duration) -> (Rc<dyn Stream<PriceBook>>, Rc<dyn Node>) {
    let src = source::market_data(period);
    let inst_price = src.inst_price;
    let aggregator =
        PriceAggregator::new(inst_price.clone(), src.new_instrument, src.del_instrument)
            .into_stream();
    // Include inst_price in the initial graph so price_ticker starts in sync
    // with event_ticker (both fire in the first engine cycle at t=0).
    (aggregator, inst_price.as_node())
}

fn main() -> anyhow::Result<()> {
    env_logger::init();
    let period = Duration::from_secs(1);
    let (aggregator, inst_price) = build(period);
    Graph::new(
        vec![aggregator.logged(">", Info).as_node(), inst_price],
        RunMode::HistoricalFrom(NanoTime::ZERO),
        RunFor::Cycles(20),
    )
    .run()
}

#[cfg(test)]
#[path = "../tests.rs"]
mod tests;
