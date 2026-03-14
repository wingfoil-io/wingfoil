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

    fn values<T: Clone>(collected: &[wingfoil::ValueAt<T>]) -> Vec<T> {
        collected.iter().map(|v| v.value.clone()).collect()
    }

    /// After 10 event ticks the lifecycle is:
    ///   adds:    n=1..4, 6..9  → inst0..inst3, inst4..inst7  (8 events)
    ///   deletes: n=5, 10       → inst0, inst1               (2 events)
    #[test]
    fn source_lifecycle_sequence() {
        let period = std::time::Duration::from_secs(1);

        let src = source(period);
        let new_insts = src.new_instrument.collect();
        new_insts
            .run(
                wingfoil::RunMode::HistoricalFrom(wingfoil::NanoTime::ZERO),
                wingfoil::RunFor::Cycles(10),
            )
            .unwrap();
        let new_vals = values(&new_insts.peek_value());
        assert_eq!(new_vals.len(), 8);
        assert_eq!(new_vals[0], "inst0");
        assert_eq!(new_vals[7], "inst7");

        let src = source(period);
        let del_insts = src.del_instrument.collect();
        del_insts
            .run(
                wingfoil::RunMode::HistoricalFrom(wingfoil::NanoTime::ZERO),
                wingfoil::RunFor::Cycles(10),
            )
            .unwrap();
        let del_vals = values(&del_insts.peek_value());
        assert_eq!(del_vals, vec!["inst0".to_string(), "inst1".to_string()]);
    }
}
