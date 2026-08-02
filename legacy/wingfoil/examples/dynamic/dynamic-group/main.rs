//! Demonstrates dynamic graph construction using `dynamic_group_stream`.
//!
//! Three streams drive the system:
//! - `new_instrument` — fires when an instrument is added
//! - `del_instrument` — fires when an instrument is deleted
//! - `inst_price`     — continuous price feed, cycling through instrument IDs
//!
//! [`dynamic_group_stream`] wires in per-instrument subgraphs on demand and
//! tears them down when an instrument is deleted.

#[path = "../market_data.rs"]
mod source;

use log::Level::Info;
use std::collections::BTreeMap;
use std::rc::Rc;
use std::time::Duration;

use wingfoil::*;

use source::{Instrument, Price};

type PriceBook = BTreeMap<Instrument, Price>;

fn process(stream: Rc<dyn Stream<(Instrument, Price)>>) -> Rc<dyn Stream<(Instrument, Price)>> {
    stream.map(|(inst, px)| (inst, (px * 100.0).round()))
}

pub fn build(period: Duration) -> (Rc<dyn Stream<PriceBook>>, Rc<dyn Node>) {
    let src = source::market_data(period);
    let inst_price = src.inst_price;
    let aggregator = dynamic_group_stream(
        src.new_instrument,
        src.del_instrument,
        {
            let inst_price = inst_price.clone();
            move |inst: Instrument| {
                let inst_key = inst.clone();
                process(inst_price.filter_value(move |(i, _)| i == &inst_key))
            }
        },
        BTreeMap::new(),
        |book: &mut PriceBook, key, (_, price)| {
            book.insert(key.clone(), price);
        },
        |book: &mut PriceBook, key| {
            book.remove(key);
        },
    );
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
