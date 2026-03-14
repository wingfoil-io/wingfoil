//! Source module: emits instrument lifecycle events and price data.

use std::rc::Rc;
use std::time::Duration;

use wingfoil::*;

pub type Instrument = String;
pub type Price = f64;

/// Bundles all three streams produced by [`source()`].
pub struct Source {
    /// Fires when a new instrument is added.
    pub new_instrument: Rc<dyn Stream<Instrument>>,
    /// Fires when an instrument is deleted.
    pub del_instrument: Rc<dyn Stream<Instrument>>,
    /// Continuous price stream, cycling through instruments.
    pub inst_price: Rc<dyn Stream<(Instrument, Price)>>,
}

/// Creates a [`Source`] with a fixed lifecycle scenario:
///
/// | Event # | Action        |
/// |---------|---------------|
/// | 1       | add  inst0    |
/// | 2       | add  inst1    |
/// | 3       | add  inst2    |
/// | 4       | add  inst3    |
/// | 5       | delete inst1  |
/// | 6       | add  inst4    |
///
/// `price_ticker` fires every `period`; `event_ticker` fires every `period * 3`.
/// `inst_price` cycles through six instrument IDs so per-instrument filters in the
/// aggregator can route prices to the right subgraph.
pub fn source(period: Duration) -> Source {
    let price_ticker = ticker(period);
    let event_ticker = ticker(period * 3);

    let lifecycle: Rc<dyn Stream<(bool, Instrument)>> =
        event_ticker.count().map(|n: u64| match n {
            1 => (true, "inst0".to_string()),
            2 => (true, "inst1".to_string()),
            3 => (true, "inst2".to_string()),
            4 => (true, "inst3".to_string()),
            5 => (false, "inst1".to_string()),
            6 => (true, "inst4".to_string()),
            _ => (true, format!("inst{n}")),
        });

    let new_instrument = lifecycle
        .clone()
        .filter_value(|(is_new, _)| *is_new)
        .map(|(_, inst)| inst);

    let del_instrument = lifecycle
        .filter_value(|(is_new, _)| !*is_new)
        .map(|(_, inst)| inst);

    let inst_price = price_ticker.count().map(|n: u64| {
        let id = (n - 1) % 6;
        (format!("inst{id}"), id as f64 + n as f64 / 100.0)
    });

    Source {
        new_instrument,
        del_instrument,
        inst_price,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn values<T: Clone>(collected: &[wingfoil::ValueAt<T>]) -> Vec<T> {
        collected.iter().map(|v| v.value.clone()).collect()
    }

    #[test]
    fn source_lifecycle_sequence() {
        let period = std::time::Duration::from_secs(1);

        // Test new instruments. Run 6 event-ticker cycles to cover all 6 lifecycle events.
        let src = source(period);
        let new_insts = src.new_instrument.collect();
        new_insts
            .run(
                wingfoil::RunMode::HistoricalFrom(wingfoil::NanoTime::ZERO),
                wingfoil::RunFor::Cycles(6),
            )
            .unwrap();
        assert_eq!(
            values(&new_insts.peek_value()),
            vec!["inst0", "inst1", "inst2", "inst3", "inst4"]
                .into_iter()
                .map(String::from)
                .collect::<Vec<_>>(),
        );

        // Test del instruments with a fresh source.
        let src = source(period);
        let del_insts = src.del_instrument.collect();
        del_insts
            .run(
                wingfoil::RunMode::HistoricalFrom(wingfoil::NanoTime::ZERO),
                wingfoil::RunFor::Cycles(6),
            )
            .unwrap();
        assert_eq!(values(&del_insts.peek_value()), vec!["inst1".to_string()]);
    }
}
