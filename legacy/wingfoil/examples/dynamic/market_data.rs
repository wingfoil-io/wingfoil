//! Source module: emits instrument lifecycle events and price data.

use std::rc::Rc;
use std::time::Duration;

use wingfoil::*;

pub type Instrument = String;
pub type Price = f64;

/// Bundles all three streams produced by [`market_data()`].
pub struct MarketData {
    /// Fires when a new instrument is added.
    #[allow(dead_code)]
    pub new_instrument: Rc<dyn Stream<Instrument>>,
    /// Fires when an instrument is deleted.
    pub del_instrument: Rc<dyn Stream<Instrument>>,
    /// Continuous price stream, cycling through instrument IDs.
    pub inst_price: Rc<dyn Stream<(Instrument, Price)>>,
}

/// State threaded through the lifecycle [`fold`].
///
/// Instruments are added sequentially. Every 3rd event deletes the oldest
/// live instrument (if any), producing an unbounded stream of additions and
/// periodic deletions.
#[derive(Clone, Debug)]
struct LifecycleState {
    /// Counter for naming the next instrument (`inst1`, `inst2`, …).
    next_id: u64,
    /// Currently live instruments, in insertion order.
    live: Vec<Instrument>,
    /// The event to emit on this tick: `(is_add, instrument)`.
    event: (bool, Instrument),
}

impl Default for LifecycleState {
    fn default() -> Self {
        Self {
            next_id: 1,
            live: Vec::new(),
            event: (true, String::new()),
        }
    }
}

/// Creates a [`Source`] driven by two tickers, both firing every `period`:
///
/// - `price_ticker` — continuous price feed, cycling through instrument IDs
///   in a sliding window: `id = (n-1)/3 + (n-1)%3 + 1`
/// - `event_ticker` — instrument lifecycle; every 3rd tick deletes the oldest
///   live instrument, all other ticks add a fresh one
///
/// Prices for unknown or deleted instruments are silently dropped by the
/// aggregator's per-instrument filters.
pub fn market_data(period: Duration) -> MarketData {
    let price_ticker = ticker(period);
    let event_ticker = ticker(period);

    let lifecycle: Rc<dyn Stream<(bool, Instrument)>> = event_ticker
        .count()
        .fold(|state: &mut LifecycleState, n: u64| {
            if n.is_multiple_of(3) && !state.live.is_empty() {
                // Delete the oldest live instrument.
                let inst = state.live.remove(0);
                state.event = (false, inst);
            } else {
                // Add a fresh instrument.
                let inst = format!("inst{}", state.next_id);
                state.next_id += 1;
                state.live.push(inst.clone());
                state.event = (true, inst);
            }
        })
        .map(|s| s.event);

    let new_instrument = lifecycle
        .clone()
        .filter_value(|(is_new, _)| *is_new)
        .map(|(_, inst)| inst);

    let del_instrument = lifecycle
        .filter_value(|(is_new, _)| !*is_new)
        .map(|(_, inst)| inst);

    let inst_price = price_ticker.count().map(|n: u64| {
        let id = (n - 1) / 3 + (n - 1) % 3 + 1;
        (format!("inst{id}"), id as f64 + n as f64 / 100.0)
    });

    MarketData {
        new_instrument,
        del_instrument,
        inst_price,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use wingfoil::Graph;

    /// 9 engine cycles, both tickers at the same period:
    ///
    ///   n=1: add  inst1          n=2: add  inst2          n=3: del  inst1
    ///   n=4: add  inst3          n=5: add  inst4          n=6: del  inst2
    ///   n=7: add  inst5          n=8: add  inst6          n=9: del  inst3
    ///
    ///   price id = (n-1)/3 + (n-1)%3 + 1  →  sliding window inst1..inst5
    #[test]
    fn source_lifecycle_sequence() {
        let period = std::time::Duration::from_secs(1);
        let src = market_data(period);

        let new_insts = src.new_instrument.accumulate().finally(|v, _| {
            assert_eq!(
                v,
                ["inst1", "inst2", "inst3", "inst4", "inst5", "inst6"]
                    .map(String::from)
                    .to_vec()
            );
            Ok(())
        });

        let del_insts = src.del_instrument.accumulate().finally(|v, _| {
            assert_eq!(v, ["inst1", "inst2", "inst3"].map(String::from).to_vec());
            Ok(())
        });

        let prices = src.inst_price.accumulate().finally(|v, _| {
            assert_eq!(
                v,
                vec![
                    ("inst1".into(), 1.01),
                    ("inst2".into(), 2.02),
                    ("inst3".into(), 3.03),
                    ("inst2".into(), 2.04),
                    ("inst3".into(), 3.05),
                    ("inst4".into(), 4.06),
                    ("inst3".into(), 3.07),
                    ("inst4".into(), 4.08),
                    ("inst5".into(), 5.09),
                ]
            );
            Ok(())
        });

        Graph::new(
            vec![new_insts, del_insts, prices],
            wingfoil::RunMode::HistoricalFrom(wingfoil::NanoTime::ZERO),
            wingfoil::RunFor::Cycles(9),
        )
        .run()
        .unwrap();
    }
}
