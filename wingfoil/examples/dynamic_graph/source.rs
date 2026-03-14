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
    /// Continuous price stream, cycling through instrument IDs.
    pub inst_price: Rc<dyn Stream<(Instrument, Price)>>,
}

/// State threaded through the lifecycle [`fold`].
///
/// Instruments are added sequentially. Every 5th event deletes the oldest
/// live instrument (if any), producing an unbounded stream of additions and
/// periodic deletions.
#[derive(Clone, Debug, Default)]
struct LifecycleState {
    /// Counter for naming the next instrument (`inst0`, `inst1`, …).
    next_id: u64,
    /// Currently live instruments, in insertion order.
    live: Vec<Instrument>,
    /// The event to emit on this tick: `(is_add, instrument)`.
    event: (bool, Instrument),
}

/// Creates a [`Source`] driven by two tickers:
///
/// - `price_ticker` (every `period`) — continuous price feed
/// - `event_ticker` (every `period * 3`) — instrument lifecycle
///
/// Lifecycle rule: every 5th event tick deletes the oldest live instrument;
/// all other ticks add a fresh one. The price stream cycles through instrument
/// IDs; prices for unknown or deleted instruments are silently dropped by the
/// aggregator's per-instrument filters.
pub fn source(period: Duration) -> Source {
    let price_ticker = ticker(period);
    let event_ticker = ticker(period * 3);

    let lifecycle: Rc<dyn Stream<(bool, Instrument)>> = event_ticker
        .count()
        .fold(|state: &mut LifecycleState, n: u64| {
            if n % 5 == 0 && !state.live.is_empty() {
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
        let id = (n - 1) % 10;
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
    use wingfoil::Graph;

    /// Lifecycle over 16 engine cycles (price every 1 s, event every 3 s, both
    /// starting at t = 0):
    ///
    ///   event n=1 (t= 0s): add  inst0
    ///   event n=2 (t= 3s): add  inst1
    ///   event n=3 (t= 6s): add  inst2
    ///   event n=4 (t= 9s): add  inst3
    ///   event n=5 (t=12s): del  inst0   ← first delete
    ///   event n=6 (t=15s): add  inst4
    ///
    ///   price ticks: 16 total (inst0..inst9 then inst0..inst5, id = (n-1) % 10)
    #[test]
    fn source_lifecycle_sequence() {
        let period = std::time::Duration::from_secs(1);
        let src = source(period);

        let new_insts = src.new_instrument.accumulate().finally(|v, _| {
            assert_eq!(
                v,
                ["inst0", "inst1", "inst2", "inst3", "inst4"]
                    .map(String::from)
                    .to_vec()
            );
            Ok(())
        });

        let del_insts = src.del_instrument.accumulate().finally(|v, _| {
            assert_eq!(v, ["inst0"].map(String::from).to_vec());
            Ok(())
        });

        let prices = src.inst_price.accumulate().finally(|v, _| {
            assert_eq!(
                v,
                vec![
                    ("inst0".into(), 0.01),
                    ("inst1".into(), 1.02),
                    ("inst2".into(), 2.03),
                    ("inst3".into(), 3.04),
                    ("inst4".into(), 4.05),
                    ("inst5".into(), 5.06),
                    ("inst6".into(), 6.07),
                    ("inst7".into(), 7.08),
                    ("inst8".into(), 8.09),
                    ("inst9".into(), 9.10),
                    ("inst0".into(), 0.11),
                    ("inst1".into(), 1.12),
                    ("inst2".into(), 2.13),
                    ("inst3".into(), 3.14),
                    ("inst4".into(), 4.15),
                    ("inst5".into(), 5.16),
                ]
            );
            Ok(())
        });

        Graph::new(
            vec![new_insts, del_insts, prices],
            wingfoil::RunMode::HistoricalFrom(wingfoil::NanoTime::ZERO),
            wingfoil::RunFor::Cycles(16),
        )
        .run()
        .unwrap();
    }
}
